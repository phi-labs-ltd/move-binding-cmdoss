use crate::package_id_resolver::PackageIdResolver;
use crate::SuiNetwork;
use fastcrypto::encoding::{Base64, Encoding};
use move_binary_format::normalized::{Module, StringPool};
use move_binary_format::CompiledModule;
use move_core_types::account_address::AccountAddress;
use move_core_types::identifier::{IdentStr, Identifier};
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

// Simple string pool implementation that uses Identifier directly
#[derive(Default)]
struct SimpleStringPool;

impl StringPool for SimpleStringPool {
    type String = Identifier;

    fn intern(&mut self, s: &IdentStr) -> Self::String {
        s.to_owned()
    }

    fn as_ident_str<'a>(&'a self, s: &'a Self::String) -> &'a IdentStr {
        s.as_ident_str()
    }
}

pub trait ModuleProvider {
    fn get_package(&self, package_id: &str) -> Result<Package, anyhow::Error>;
}

pub struct MoveModuleProvider {
    network: SuiNetwork,
}

impl MoveModuleProvider {
    pub fn new(network: SuiNetwork) -> Self {
        Self { network }
    }
}

impl ModuleProvider for MoveModuleProvider {
    fn get_package(&self, package: &str) -> Result<Package, anyhow::Error> {
        let package_id = PackageIdResolver::resolve_package_id(self.network, package)?;
        let client = reqwest::blocking::Client::new();
        let request = format!(
            r#"{{package(address: "{package_id}") {{moduleBcs, typeOrigins{{module, struct, definingId}}, version}}}}"#
        );
        let res = client
            .post(self.network.gql())
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "query": request,
                "variables": Value::Null
            }))
            .send()
            .ok()
            .expect("Error fetching package from Sui GQL.");

        let value = res.json::<Value>().unwrap();

        // Check if package exists
        if value["data"]["package"].is_null() {
            return Err(anyhow::anyhow!(
                "Package {} not found on {}. The package may not exist or has been removed.",
                package_id,
                self.network.gql()
            ));
        }

        let module_bcs: String =
            serde_json::from_value(value["data"]["package"]["moduleBcs"].clone()).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse moduleBcs for package {}: {}. Response: {:?}",
                    package_id,
                    e,
                    value
                )
            })?;
        let module_bytes = Base64::decode(&module_bcs).unwrap();
        let module_map: BTreeMap<String, Vec<u8>> = bcs::from_bytes(&module_bytes).unwrap();

        let module_map = module_map
            .iter()
            .map(|(name, bytes)| {
                let module = CompiledModule::deserialize_with_defaults(bytes)?;
                let mut pool = SimpleStringPool::default();
                let normalized = Module::new(&mut pool, &module, false);
                Ok::<_, anyhow::Error>((name.clone(), normalized))
            })
            .collect::<Result<_, _>>()?;

        let type_origin_table: Vec<Value> =
            serde_json::from_value(value["data"]["package"]["typeOrigins"].clone())?;

        let type_origin_table = type_origin_table.iter().fold(
            HashMap::new(),
            |mut results: HashMap<String, HashMap<String, AccountAddress>>, v| {
                // Skip entries with null values
                if let (Some(module), Some(struct_), Some(defining_id)) = (
                    v["module"].as_str(),
                    v["struct"].as_str(),
                    v["definingId"].as_str(),
                ) {
                    if let Ok(addr) = AccountAddress::from_str(defining_id) {
                        results
                            .entry(module.to_string())
                            .or_default()
                            .insert(struct_.to_string(), addr);
                    }
                }
                results
            },
        );

        let version = serde_json::from_value(value["data"]["package"]["version"].clone())?;

        Ok(Package {
            module_map,
            type_origin_table,
            version,
        })
    }
}

pub struct Package {
    pub module_map: BTreeMap<String, Module<Identifier>>,
    pub type_origin_table: HashMap<String, HashMap<String, AccountAddress>>,
    pub version: u64,
}

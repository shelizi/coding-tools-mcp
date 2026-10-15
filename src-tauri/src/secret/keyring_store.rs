use crate::data::DataStore;
use crate::error::AppResult;

pub struct SecretStore;

impl SecretStore {
    #[cfg(test)]
    pub fn remove_workspace_secrets(profile_id: &str) -> AppResult<()> {
        let mut store = DataStore::load()?;
        store.remove_workspace_secrets(profile_id)
    }

    pub fn set(profile_id: &str, key: &str, value: &str) -> AppResult<()> {
        DataStore::write_workspace_secret_latest(profile_id, key, value)
    }

    pub fn get(profile_id: &str, key: &str) -> AppResult<Option<String>> {
        DataStore::read_workspace_secret_latest(profile_id, key)
    }

    pub fn regenerate(profile_id: &str, key: &str) -> AppResult<String> {
        let value = random_secret();
        Self::set(profile_id, key, &value)?;
        Ok(value)
    }

    pub fn get_shared(key: &str) -> AppResult<Option<String>> {
        DataStore::read_file(|data| Ok(data.shared_secrets.get(key).cloned()))
    }

    pub fn set_shared(key: &str, value: &str) -> AppResult<()> {
        DataStore::write_shared_secret_latest(key, value)
    }

    pub fn get_app(scope: &str, item_id: &str) -> AppResult<Option<String>> {
        DataStore::read_file(|data| {
            Ok(data
                .app_secrets
                .get(scope)
                .and_then(|items| items.get(item_id))
                .filter(|value| !value.is_empty())
                .cloned())
        })
    }

    pub fn set_app(scope: &str, item_id: &str, value: &str) -> AppResult<()> {
        DataStore::write_app_secret_latest(scope, item_id, value)
    }
}

fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).replace('-', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_secret_is_non_empty() {
        assert!(random_secret().len() > 32);
    }

    #[test]
    fn workspace_secret_roundtrip() {
        let id = uuid::Uuid::new_v4().to_string().replace('-', "");
        SecretStore::set(&id, "oauth_client_secret", "roundtrip-secret").expect("set");
        let loaded = SecretStore::get(&id, "oauth_client_secret").expect("get");
        assert_eq!(loaded.as_deref(), Some("roundtrip-secret"));
        let _ = SecretStore::remove_workspace_secrets(&id);
    }
}

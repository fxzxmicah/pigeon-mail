use std::fs;
use std::path::Path;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

pub fn load_vec<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&content)?)
}

pub fn save_slice<T: Serialize>(path: &Path, values: &[T]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("JSON store path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(values)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load_vec, save_slice};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Record {
        id: String,
        enabled: bool,
    }

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("json-store-test-{}", glib::uuid_string_random()))
            .join(name)
    }

    #[test]
    fn missing_and_empty_files_load_as_empty_collections() {
        let path = fixture_path("records.json");
        assert!(load_vec::<Record>(&path).unwrap().is_empty());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, " \n").unwrap();
        assert!(load_vec::<Record>(&path).unwrap().is_empty());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_replaces_the_complete_document_and_round_trips_values() {
        let path = fixture_path("records.json");
        save_slice(
            &path,
            &[
                Record {
                    id: "first".into(),
                    enabled: true,
                },
                Record {
                    id: "second".into(),
                    enabled: false,
                },
            ],
        )
        .unwrap();
        save_slice(
            &path,
            &[Record {
                id: "replacement".into(),
                enabled: true,
            }],
        )
        .unwrap();

        assert_eq!(
            load_vec::<Record>(&path).unwrap(),
            [Record {
                id: "replacement".into(),
                enabled: true,
            }]
        );
        assert!(!path.with_extension("json.tmp").exists());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn malformed_json_is_reported_instead_of_silently_discarded() {
        let path = fixture_path("records.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[{\"id\":}").unwrap();

        assert!(load_vec::<Record>(&path).is_err());

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

use toml;
use moi::*;
use std::path::Path;

pub fn gets<'a>(config: &'a toml::Value, key: &str) -> BoxResult<Option<&'a str>> {
    Ok(match config.get(key) {
        None => None,
        Some(v) => Some(v.as_str().or_then_err(|| format!("value of '{}' is not a string",key))?)
    })    
}

pub fn gets_or<'a>(config: &'a toml::Value, key: &str, def: &'static str) -> BoxResult<&'a str> {
    Ok(match gets(config,key)? {
        Some(res) => res,
        None => def
    })
}

pub fn geti_or(config: &toml::Value, key: &str, def: i64) -> BoxResult<i64> {
    Ok(match config.get(key) {
        None => def,
        Some(v) => v.as_integer().or_then_err(|| format!("value of '{}' is not an integer",key))?
    })
}

pub fn maybe_toml_config(name: &str, home: &Path) -> BoxResult<Option<toml::Value>> {
    let path = Path::new(name).with_extension("toml");
    let maybe_path = if path.exists() {
        Some(path)
    } else {
        let path = home.join(name).with_extension("toml");
        if path.exists() {
            Some(path)
        } else {
            None
        }
    };
    if let Some(path) = maybe_path {
        let toml: toml::Value = read_to_string(&path)?.parse()?;
        Ok(Some(toml))
    } else {
        Ok(None)
    }
}

pub fn toml_strings (t: &Vec<toml::Value>) -> BoxResult<Vec<String>> {
    // TODO that unwrap is bad, man! Return a result properly
    Ok(t.into_iter().map(|s| s.as_str().unwrap().to_string()).collect())
}


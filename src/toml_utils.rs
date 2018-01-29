use toml;
use super::*;
use std::path::Path;

pub fn as_string(v: &toml::Value) -> BoxResult<String> {
    v.as_str().or_err("not a string").map_err(|e| e.into()).map(|s| s.into())
}

pub fn gets_opt<'a>(config: &'a toml::Value, key: &str) -> BoxResult<Option<&'a str>> {
    Ok(match config.get(key) {
        None => None,
        Some(v) => Some(v.as_str().or_then_err(|| format!("value of '{}' is not a string",key))?)
    })
}

pub fn gets<'a>(config: &'a toml::Value, key: &str) -> BoxResult<&'a str> {
    match gets_opt(config,key)? {
        Some(s) => Ok(s),
        None => err_io(&format!("{} not found",key))
    }
}

pub fn gets_or<'a>(config: &'a toml::Value, key: &str, def: &'static str) -> BoxResult<&'a str> {
    Ok(match gets_opt(config,key)? {
        Some(res) => res,
        None => def
    })
}

pub fn gets_or_then<'a,C: FnOnce()->String>(config: &'a toml::Value, key: &str, def: C) -> BoxResult<String> {
    Ok(match gets_opt(config,key)? {
        Some(res) => res.to_string(),
        None => def()
    })
}

pub fn geti_or(config: &toml::Value, key: &str, def: i64) -> BoxResult<i64> {
    Ok(match config.get(key) {
        None => def,
        Some(v) => v.as_integer().or_then_err(|| format!("value of '{}' is not an integer",key))?
    })
}

pub fn maybe_toml_config(name: &str, home: &Path) -> BoxResult<Option<(bool,toml::Value)>> {
    let path = Path::new(name).with_extension("toml");
    let (local,maybe_path) = if path.exists() {
        (true,Some(path))
    } else {
        let path = home.join(name).with_extension("toml");
        if path.exists() {
            (false,Some(path))
        } else {
            (false,None)
        }
    };
    if let Some(path) = maybe_path {
        let toml: toml::Value = read_to_string(&path)?.parse()?;
        Ok(Some((local,toml)))
    } else {
        Ok(None)
    }
}

pub fn toml_strings (t: &Vec<toml::Value>) -> BoxResult<Vec<String>> {
    t.into_iter().map(|s| as_string(s)).collect()
}


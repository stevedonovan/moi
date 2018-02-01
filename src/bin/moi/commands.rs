use flags::*;
use moi::*;
use toml;
use std::fs;
use std::path::Path;
use toml_utils::*;

pub struct CommandHandler<'a> {
    flags: &'a Flags,
    store: &'a Config,
    config: &'a toml::Value,
}

impl <'a> CommandHandler<'a> {
    pub fn new(flags: &'a Flags, store: &'a Config, config: &'a toml::Value) -> CommandHandler<'a> {
        CommandHandler {
            flags: flags,
            store: store,
            config: config,
        }
    }

    pub fn groups(&self) -> BoxResult<bool> {
        if let Some(groups) = self.store.values.get("groups") {
            for (name,members) in groups.entries() {
                if self.flags.verbose {
                    println!("{}:",name);
                    for (addr,name) in members.entries() {
                        println!("\t{}\t{}",addr,name);
                    }
                } else {
                    println!("{} {} members",name,members.len());
                }
            }
        } else {
            println!("no groups yet!");
        }
        Ok(true)
    }

    fn toml_commands(&self, name: &str, t: &toml::Value) -> BoxResult<()> {
        if t.get("command").is_some() || t.get("stages").is_some() {
            let help = gets_or(t,"help","<no help>")?;
            println!("{}: {}", name,help);
        }
        Ok(())
    }

    fn toml_directory(&self, dir: &Path) -> BoxResult<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "toml" {
                    let t: toml::Value = read_to_string(&path)?.parse()?;
                    let path = path.with_extension("");
                    let fname = path.file_name().unwrap().to_str().unwrap();
                    self.toml_commands(fname,&t)?;
                }
            }
        }
        Ok(())
    }

    pub fn custom_commands(&self) -> BoxResult<bool>  {
        if let Some(config_cmds) = self.config.get("commands") {
            if config_cmds.is_table() {
                let table = config_cmds.as_table().unwrap();
                for (name,c) in table.iter() {
                    self.toml_commands(name,c)?;
                }
            }
        }
        self.toml_directory(&self.flags.moi_dir)?;
        self.toml_directory(Path::new("."))?;
        Ok(true)
    }
}


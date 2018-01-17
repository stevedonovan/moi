use flags::*;
use moi::*;

pub fn handle_local_command(cmds: &[CommandArgs], flags: &Flags, store: &Config) -> bool {
    match cmds[0].command.as_str() {
        "groups" => {
            if let Some(groups) = store.values.get("groups") {
                for (name,members) in groups.entries() {
                    if flags.verbose {
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
            return true;
        },
        "recipes" => {

            return true;
        }
        _ => return false
    }
}

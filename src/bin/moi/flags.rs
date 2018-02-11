use lapp;
use moi::*;
use toml;
use libc;
use strutil;
use query::*;
use toml_utils::*;
use std::{env,mem,fs,process};
use std::path::PathBuf;
use std::path::Path;
use std::time::Instant;
use std::collections::HashMap;
use std::os::unix::fs::DirBuilderExt;
use io::prelude::*;

use ansi_term::Colour::Yellow;

const VERSION: &str = "0.1.6";

const USAGE: &str = "
Execute commands on devices
  -V, --version version of MOI
  -c, --config (path default ~/.local/moi/config.toml) configuration file
  -f, --filter (default none) only for the selected devices
            KEY test for existence of key
            KEY=VALUE  test for equality
            KEY=VAL#   test for values that start with given string
            KEY:VALUE  test whether value is in the array KEY
            KEY.not.VALUE test for values not equal to VALUE
  -g, --group (default none) for a predefined group
  -n, --name (default none) for either address, name or group
  -T, --timeout (default 500) timeout for accessing all devices
  -j, --json  JSON output
  -v, --verbose tell us all about what's going on...
  -q, --quiet output only on error
  -m, --message-format (default plain) one of plain,csv or json
  <command> (string)
        ls <keys>: display values of keys (defaults to 'addr','name')
        run cmd [pwd]: run command remotely
        launch cmd [pwd]: like run - use instead when command can take a long time
        push file dest: copy a file to a remote destination
        push-run file dest cmd: copy a file and run a command
        pull file dest: copy remote files to us
        run-pull cmd file dest: run a command and then copy the result
        set key=value...:  set keys on remotes
        seta key=value...: append values to array-valued keys
        group name: create a group from the set of responses
        remove-group: remove a named group from the set
        groups: show defined groups
        ping:  like ls, but gives round-trip time in msec
        time:  like ls, but gives difference between this time and device time, in secs
  <args> (string...) additional arguments for commands
";

// ISSUE: moi otherwise respects PREFIX, except here
// Must be overridable by custom install
const INSTALL_PREFIX: &str = "/usr/local/etc";

fn moi_su_dir() -> PathBuf {
    Path::new(INSTALL_PREFIX).join("moi")
}

pub struct CommandArgs {
    pub command: String,
    pub arguments: Vec<String>,
}

pub struct Flags {
    pub filter_desc: String,
    pub group_name: String,
    pub name_or_group: String,
    pub config_file: PathBuf,
    pub moi_dir: PathBuf,
    pub json_store: PathBuf,
    pub timeout: i32,
    pub verbose: bool,
    pub quiet: bool,
    pub su: bool,
    pub sharing_with_su: bool,
    pub json: bool,
   // format: String,
}

impl Flags {
    pub fn new() -> BoxResult<(Vec<CommandArgs>,Flags)> {
        let args = lapp::parse_args(USAGE);
        if args.get_bool("version") {
            println!("MOI comand-line interface version {}",VERSION);
            process::exit(0);
        }

        let (root,base_dir,sharing) = if unsafe { libc::geteuid() == 0 } {
            (true,INSTALL_PREFIX.into(),false)
        } else {
            // if there is ALREADY a su install of moi then
            // we need to reuse that configuration!
            let (dir,sharing) = if moi_su_dir().exists() {
                (INSTALL_PREFIX.into(), true)
            } else {
                (env::home_dir().unwrap().join(".local"), false)
            };
            (false,dir,sharing)
        };

        let moi_dir = base_dir.join("moi");
        let default_config = moi_dir.join("config.toml");
        let json_store = moi_dir.join("store.json");
        if ! moi_dir.exists() {
            fs::DirBuilder::new().recursive(true).create(&moi_dir)?;
            let mut f = fs::File::create(&default_config)?;
            write!(f,"[config]\nmqtt_addr = \"localhost\"\n")?;
            write!(f,"restricted = \"{}\"\n",if root {"yes"} else {"no"})?;
            write_all(&json_store,"{}\n")?;
            let text = format!("Creating {}.\nEdit mqtt_addr and restricted if necessary",default_config.display());
            println!("{}",Yellow.bold().paint(text));
            if root {
                let certs = moi_dir.join("certs");
                fs::DirBuilder::new().mode(0o700).create(&certs)?;
                let text = format!("Default certificate dir {} created",certs.display());
                println!("{}",Yellow.bold().paint(text));
            }
        }

        let command = args.get_string("command");
        let mut arguments = args.get_strings("args");
        arguments.insert(0,command);

        let mut commands = Vec::new();
        {
            let mut push = |mut aa: Vec<String>| {
                if aa.len() == 0 { args.quit("must have at least one value after ::"); }
                commands.push(CommandArgs{command: aa.remove(0), arguments: aa});
            };
            let mut this_chunk = Vec::new();
            for s in arguments {
                if s == "::" {
                    let mut tmp = Vec::new();
                    mem::swap(&mut this_chunk, &mut tmp);
                    push(tmp);
                    // this_chunk is now a new empty vector
                } else {
                    this_chunk.push(s);
                }
            }
            push(this_chunk);
        }

        Ok((commands,Flags {
            filter_desc: args.get_string("filter"),
            group_name: args.get_string("group"),
            name_or_group: args.get_string("name"),
            timeout: args.get_integer("timeout"),
            verbose: args.get_bool("verbose"),
            quiet: args.get_bool("quiet"),
            config_file: if root || sharing {default_config} else {args.get_path("config")},
            json_store: json_store,
            moi_dir: moi_dir,
            su: root,
            sharing_with_su: sharing,
            json: args.get_bool("json"),
        }))

    }

    ///// creating queries out of command-line args //////

    fn keyvalue_args(args: &[String]) -> BoxResult<HashMap<String,String>> {
        use strutil::split_at_delim;
        let mut map = HashMap::new();
        for s in args {
            let (k,v) = split_at_delim(s,"=")
                .or_then_err(|| format!("{} is not a key-value pair",s))?;
            KeyValue::valid_key(k)
                .or_then_err(|| format!("{} is not a valid key name",k))?;
            map.insert(k.to_string(),v.to_string());
        }
        Ok(map)
    }

    fn remote_target_destination<'a>(&mut self, spec: &'a str) -> BoxResult<&'a str> {
        Ok(if let Some((target,dest)) = strutil::split_at_delim(spec,":") {
            (self.name_or_group == "none" || self.name_or_group == target).or_err("can only specify target once")?;
            self.name_or_group = target.into();
            dest
        } else {
            spec
        })
    }

    // implement our commands as Query enum values
    fn construct_query(&mut self, cmd: &str, args: &[String], restricted: bool) -> BoxResult<Query> {
        use strutil::strings;
        if restricted && ! &["ls","time","ping"].contains(&cmd) {
            return err_io(&format!("{} is a restricted command. Use sudo",cmd));
        }
        match cmd {
            "ls" => {
                Ok(Query::get(args,cmd.into()))
            },
            "time" => {
                Ok(Query::get(&["time"],cmd.into()))
            },
            "ping" => {
                Ok(Query::Ping(Instant::now()))
            },
            "group" => {
                (args.len() == 1).or_err("group: group-name")?;
                Ok(Query::group(&args[0]))
            },
            "set" | "seta" => {
                (args.len() > 0).or_then_err(|| format!("{}: key1=value1 [key2=value2 ...]",cmd))?;
                let map = Flags::keyvalue_args(args)?;
                Ok(if cmd=="set" {Query::Set(map)} else {Query::Seta(map)})
            },
            "invoke" => {
                (args.len() > 0).or_err("invoke: custom-command [key1=value1 ...]")?;
                let name = args[0].clone();
                let map = Flags::keyvalue_args(&args[1..])?;
                Ok(Query::Invoke(name,map))
            },
            "remove-group" => {
                (args.len() == 1).or_err("remove-group: group-name")?;
                Ok(Query::rma("groups",&args[0]))
            },
            "run" | "launch" | "spawn" => {
                (args.len() >= 1).or_then_err(|| format!("{}: command [working-dir] [job-name]",cmd))?;
                let working_dir = if let Some(working_dir) = args.get(1) {
                    Some(self.remote_target_destination(working_dir)?.into())
                } else {
                    None
                };
                let rc = RunCommand::new(&args[0],working_dir,args.get(2).cloned());
                Ok(
                    if cmd=="run" {Query::Run(rc)}
                    else if cmd=="launch" {Query::Launch(rc)}
                    else {Query::Spawn(rc)}
                )
            },
            "wait" => Ok(Query::Wait),
            "push" => {
                (args.len() == 2).or_err("push: local-file-name remote-dest")?;
                let path = PathBuf::from(args[0].clone());
                (path.exists() && path.is_file()).or_err("push: file does not exist, or is a directory")?;
                let dest = self.remote_target_destination(&args[1])?;
                let mut cf = CopyFile::new(
                    path,
                    dest,
                )?;
                cf.read_bytes()?;
                Ok(Query::Copy(cf))
            },
            "pull" => {
                (args.len() == 2).or_err("pull: remote-file-name local-dest")?;
                let dest = self.remote_target_destination(&args[0])?;
                let remote_path = PathBuf::from(dest);
                let mut local_path = PathBuf::from(&args[1]);
                if local_path.is_dir() {
                    local_path.push(&format!("%n-%a-{}",remote_path.file_name().unwrap().to_str().unwrap()));
                }
                {
                    let parent = local_path.parent()
                        .or_then_err(|| format!("pull: destination {} has no parent",local_path.display()))?;
                    if parent != Path::new("") {
                        writeable_directory(&parent)?;
                    }
                }
                Ok(Query::Fetch(FetchFile {
                    source: remote_path,
                    local_dest: local_path,
                }))
            },
            "push-run" => {
                // example of a two-step command
                (args.len() == 3).or_err("push-run: local-file destination command")?;
                let file = &args[0];
                let dest = &args[1];
                let cmd = &args[2];
                Ok(Query::Actions(vec![
                    self.construct_query("push",&strings(&[file,dest]),restricted)?,
                    self.construct_query("run",&strings(&[cmd,dest]),restricted)? // use dest as pwd
                ]))
            },
            "run-pull" => {
                (args.len() == 3).or_err("run-pull: command dir remote-file")?;
                let cmd = &args[0];
                let dir = &args[1];
                let file = &args[2];
                Ok(Query::Actions(vec![
                    self.construct_query("run",&strings(&[cmd,dir]),restricted)?,
                    self.construct_query("pull",&strings(&[file,dir]),restricted)?
                ]))
            },
            "restart" => {
                Ok(Query::Restart(0))
            },
            _ => {
                err_io(&format!("not a command: {}",cmd))
            }
        }
    }

    fn query_alias(&mut self, def: &toml::Value, config: &toml::Value, cmd: &CommandArgs, help: &str, restricted: bool) -> BoxResult<Query> {
        if let Some(alias) = def.get("alias") {
            let alias = alias.as_str().or_err("alias: alias must be string")?;
            let real_alias = strutil::replace_dollar_args(alias,&cmd.arguments)?;
            let cmds = &[CommandArgs{command: real_alias, arguments: cmd.arguments.clone()}];
            Ok(self.construct_query_alias(config,cmds,restricted)?)
        } else {
            // MUST have at least "command" and "args"
            let alias_command = def.get("command").or_err("alias: command must be defined")?
                .as_str().or_err("alias: command must be string")?;

            let alias_args = toml_strings(def.get("args").or_err("alias: args must be defined")?
                .as_array().or_err("alias: args must be array")?
            )?;
            let alias_args = strutil::replace_dollar_args_array(&alias_args,&cmd.arguments)
                .map_err(|e| io_error(&format!("{} '{}' - {}",cmd.command, help, e)))?;

            if self.verbose {
                println!("alias command {} args {:?}",alias_command,alias_args);
            }
            let cmds = &[CommandArgs{command: alias_command.into(), arguments: alias_args}];
            Ok(self.construct_query_alias(config,cmds,restricted)?)
        }
    }

    fn query_alias_collect(&mut self, t: &toml::Value, config: &toml::Value, cmd: &CommandArgs, res: &mut Vec<Query>, restricted: bool) -> BoxResult<()> {
        // the filter and/or the group can be overriden, but currently only in the first command
        // of a sequence
        if let Some(filter) = gets_opt(t,"filter")? {
            self.filter_desc = strutil::replace_dollar_args(filter,&cmd.arguments)?;
            if self.verbose {
                println!("alias set filter {}",self.filter_desc);
            }
        }
        if let Some(group) = gets_opt(t,"group")? {
            self.group_name = group.into();
            if self.verbose {
                println!("alias set group {}",self.group_name);
            }
        }
        if let Some(_) = t.get("quiet") {
            self.quiet = true;
        }

        // it's a cool thing to help people.
        let help = gets_or(t,"help","<no help>")?;
        // there may be multiple stages, so sections [commands.NAME.1], [commands.NAME.2]... etc in config
        let stages = geti_or(t,"stages",0)?;
        if stages == 0 {
            res.push(self.query_alias(t,config,cmd,help,restricted)?);
        } else {
            for i in 1..stages+1 {
                let idx = i.to_string();
                let sub = t.get(&idx).or_then_err(|| format!("stage {} not found",idx))?;
                res.push(self.query_alias(sub,config,cmd,help,restricted)?);
            }
        }
        Ok(())
    }

    // Program arguments passed as mutable reference, because
    // command aliases MAY modify the filter or group value
    pub fn construct_query_alias(&mut self, config: &toml::Value, commands: &[CommandArgs], restricted: bool) -> BoxResult<Query> {
        let mut res = Vec::new();
        for cmd in commands.iter() {
            let mut was_alias = false;
            // there is a section [commands.NAME] in the config TOML
            if let Some(ref lookup) = config.get("commands") {
                if let Some(t) = lookup.get(&cmd.command) { // we have an alias!
                    // these are not restricted operations
                    self.query_alias_collect(t,&config,cmd,&mut res,false)?;
                    was_alias = true;
                }
            }
            // OK, maybe the command NAME is NAME.toml or ~/.local/moi/NAME.toml
            if ! was_alias {
                if let Some((local,toml)) = maybe_toml_config(&cmd.command,&self.moi_dir)? {
                    // local alias must respect restricted mode
                    let actual_restriction = if restricted {
                        local
                    } else {
                        false
                    };
                    self.query_alias_collect(&toml,&config,cmd,&mut res, actual_restriction)?;
                    was_alias = true;
                }
            }
            // regular plain jane arguments - will complain if not recognized
            if ! was_alias {
                res.push(self.construct_query(&cmd.command, &cmd.arguments, restricted)?)
            }
        }

        // we can pack multiple queries into Actions,
        // but pass through single queries as is
        Ok(if res.len() == 1 {
            res.remove(0)
        } else {
            Query::Actions(res)
        })
    }

}

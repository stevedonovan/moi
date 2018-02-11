// miscelaneous string handling things
use moi::*;

pub fn is_ipv4(addr: &str) -> bool {
    let res: Result<Vec<_>,_> = addr.split('.').map(|p| p.parse::<u32>()).collect();
    res.is_ok()
}

pub fn strings<T: ToString>(slice: &[T]) -> Vec<String> {
    slice.iter().map(|s| s.to_string()).collect()
}

pub fn split_at_delim<'a>(txt: &'a str, delim: &str) -> Option<(&'a str,&'a str)> {
    if let Some(idx) = txt.find(delim) {
        Some((&txt[0..idx], &txt[idx+delim.len() ..]))
    } else {
        None
    }
}

pub fn replace_percent_patterns<F>(text: &str, startc: char, lookup: F) -> BoxResult<String>
where F: Fn(String,Option<String>) -> BoxResult<String> {
    const NO_CLOSING: &str = "no closing parens";
    let mut s = text;
    let mut res = String::new();
    while let Some(pos) = s.find(startc) {
        res.push_str(&s[0..pos]);
        s = &s[pos+1..]; // skip $ or %
        if s.starts_with(startc) {
            res.push(startc);
            s = &s[1..];
            continue;
        }
        let mut chars = s.chars();
        // Either $N,
        let mut extra = None;
        let mut skip = 1;
        let mut ch = chars.next().or_then_err(|| format!("{} at end of subst",startc))?;
        if ch == '(' { // $(N) or $(N:OP)
            ch = chars.next().or_err(NO_CLOSING)?;
            let next = chars.next().or_err(NO_CLOSING)?;
            if next == ':' {
                let kind: String = chars.take_while(|&c| c != ')').collect();
                skip += kind.len() + 3; // +1 for )
                extra = Some(kind);
            }
        }
        let subst = lookup(ch.to_string(),extra)?;
        res.push_str(&subst);
        s = &s[skip..]; // skip index (and maybe op)
    }
    res.push_str(s); // what's remaining
    Ok(res)
}

pub fn replace_percent_destination(text: &str, addr: &str, name: &str) -> BoxResult<String> {
    replace_percent_patterns(text, '%', |s,_| {
        Ok (
            if s == "a" {
                addr.into()
            } else if s == "n" {
                name.into()
            } else if s == "t" {
                current_time_as_secs().to_string()
            } else {
                return err_io(&format!("%{} is not recognized in destination filenames",s));
            }
        )
    })
}

fn basename(arg: &str) -> &str {
    if let Some(pos) = arg.rfind('/') {
        (&arg[pos+1..])
    } else {
        arg
    }
}

fn filestem(arg: &str) -> &str {
    let arg = basename(arg);
    if let Some(pos) = arg.rfind('.') {
        let mut stem = &arg[0..pos];
        if stem.ends_with(".tar") {
            stem = filestem(stem);
        }
        stem
    } else {
        arg
    }
}

// we split at _ or - when followed by a digit...
fn split_version(name: &str) -> Option<(&str,&str)> {
    let mut p = 0;
    let name = filestem(name);
    while let Some(pos) = (&name[p..]).find(|c:char| c=='-' || c=='_') {
        let condn = (&name[p+pos+1..]).chars().next().unwrap().is_digit(10);
        if condn  {
            return Some((&name[0..p+pos], &name[p+pos+1..]));
        } else {
            p += pos + 1;
        }
    }
    None
}

fn massage_valid_key(name: &str) -> String {
    name.chars().filter(|&c| c != '.').collect()
}

pub fn replace_dollar_args(text: &str, args: &[String]) -> BoxResult<String> {
    replace_percent_patterns(text, '$', |s,x| {
        let idx: usize = s.parse()?;
        (idx <= args.len()).or_then_err(|| format!("index %{} out of range: ({} arguments given)",idx,args.len()))?;
        let arg = args[idx-1].clone();
        if let Some(kind) = x {
            if kind == "package" {
                if let Some((name,_)) = split_version(&arg) {
                    let massaged = massage_valid_key(name);
                    if massaged != name {
                        warn!("warning: package '{}' replaced with valid key '{}'",name,massaged);
                    }
                    return Ok(massaged);
                }
            } else
            if kind == "version" {
                return Ok(if let Some((_,vs)) = split_version(&arg) {
                    vs.into()
                } else {
                    "".into()
                });
            } else
            if kind == "base" {
                return Ok(basename(&arg).into());
            } else
            if kind == "stem" {
                return Ok(filestem(&arg).into());
            } else {
                return err_io(&format!("substitution invalid kind {}",kind));
            }
        }
        Ok(arg)
    })
}

pub fn replace_dollar_args_array(strings: &[String], args: &[String]) -> BoxResult<Vec<String>> {
    let mut res = Vec::new();
    for text in strings.iter() {
        res.push(replace_dollar_args(text,args)?);
    }
    Ok(res)
}


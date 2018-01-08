// miscelaneous string handling things
use moi::*;

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

pub fn replace_percent_patterns<F>(text: &str, lookup: F) -> BoxResult<String>
where F: Fn(String) -> BoxResult<String> {
    let mut s = text;
    let mut res = String::new();
    while let Some(pos) = s.find('%') {
        res.push_str(&s[0..pos]);
        s = &s[pos+1..]; // skip &
        let ch = s.chars().next().or_then_err(|| format!("% at end of subst"))?;
        let subst = lookup(ch.to_string())?;
        res.push_str(&subst);
        s = &s[1..]; // skip index
    }
    res.push_str(s); // what's remaining
    Ok(res)
}

pub fn replace_percent_destination(text: &str, addr: &str, name: &str) -> BoxResult<String> {
    replace_percent_patterns(text, |s| {
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

pub fn replace_percent_args(text: &str, args: &[String]) -> BoxResult<String> {
    replace_percent_patterns(text, |s| {
        let idx: usize = s.parse()?;
        (idx == args.len()).or_then_err(|| format!("index %{} must be from 1 to {}",idx,args.len()))?;
        Ok(args[idx-1].clone())
    })
}

pub fn replace_percent_args_array(strings: &[String], args: &[String]) -> BoxResult<Vec<String>> {
    let mut res = Vec::new();
    for text in strings.iter() {
        res.push(replace_percent_args(text,args)?);
    }
    Ok(res)
}

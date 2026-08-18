use crate::{cli, Result};

pub fn run<I>(args: I) -> Result<String>
where
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        return Ok(cli::help_text());
    }
    cli::run(args)
}

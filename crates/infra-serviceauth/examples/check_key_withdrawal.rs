use std::{env, fs};

use infra_serviceauth::TrustRegistry;

#[expect(clippy::print_stdout, reason = "report a successful withdrawal check")]
fn main() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let (Some(previous), Some(candidate), Some(caller), kid, None) = (
        args.next(),
        args.next(),
        args.next(),
        args.next(),
        args.next(),
    ) else {
        return Err(
            "usage: check_key_withdrawal PREVIOUS.json CANDIDATE.json OLD_CALLER [OLD_KID]"
                .to_owned(),
        );
    };
    let caller = caller.to_str().ok_or("invalid caller selector")?;
    let kid = kid
        .as_deref()
        .map(|value| value.to_str().ok_or("invalid kid selector"))
        .transpose()?;
    let previous = fs::read_to_string(previous).map_err(|_| "cannot read previous registry")?;
    let candidate = fs::read_to_string(candidate).map_err(|_| "cannot read candidate registry")?;
    let candidate =
        TrustRegistry::parse(&candidate).map_err(|error| format!("candidate registry: {error}"))?;
    candidate
        .check_key_withdrawal(&previous, caller, kid)
        .map_err(|error| error.to_string())?;
    println!("Selected public key material is absent from the candidate registry.");
    Ok(())
}

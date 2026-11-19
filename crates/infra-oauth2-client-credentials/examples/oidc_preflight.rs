use infra_oauth2_client_credentials::{IssuerUrl, TokenUrl, verify_oidc_metadata};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let (Some(issuer), Some(endpoint), None) = (args.next(), args.next(), args.next()) else {
        return Err("usage: oidc_preflight ISSUER TOKEN_ENDPOINT".to_owned());
    };
    let (Some(issuer), Some(endpoint)) = (issuer.to_str(), endpoint.to_str()) else {
        return Err("URLs must be UTF-8".to_owned());
    };
    let issuer = IssuerUrl::parse(issuer).map_err(|error| error.to_string())?;
    let endpoint = TokenUrl::parse(endpoint).map_err(|error| error.to_string())?;
    verify_oidc_metadata(&issuer, &endpoint)
        .await
        .map_err(|error| error.to_string())
}

use infra_oauth2_client_credentials::verify_keycloak_metadata;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let (Some(issuer), Some(endpoint), None) = (args.next(), args.next(), args.next()) else {
        return Err("usage: keycloak_preflight ISSUER TOKEN_ENDPOINT".to_owned());
    };
    let (Some(issuer), Some(endpoint)) = (issuer.to_str(), endpoint.to_str()) else {
        return Err("URLs must be UTF-8".to_owned());
    };
    verify_keycloak_metadata(issuer, endpoint)
        .await
        .map_err(|error| error.to_string())
}

use http::{Request, header::AUTHORIZATION};

use crate::OnBehalfOf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityMode {
    Service,
    OnBehalfOf,
}

#[derive(Clone, Copy)]
pub(crate) enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
    UnexpectedSubject,
}

pub(crate) fn admit_request_subject<B>(
    request: &mut Request<B>,
    mode: IdentityMode,
) -> Result<Option<OnBehalfOf>, AdmissionRefusal> {
    if request.headers().contains_key(AUTHORIZATION) {
        return Err(AdmissionRefusal::AuthorizationConflict);
    }
    match (mode, request.extensions_mut().remove::<OnBehalfOf>()) {
        (IdentityMode::Service, Some(_)) => Err(AdmissionRefusal::UnexpectedSubject),
        (IdentityMode::OnBehalfOf, None) => Err(AdmissionRefusal::SubjectRequired),
        (IdentityMode::Service, None) => Ok(None),
        (IdentityMode::OnBehalfOf, Some(subject)) => Ok(Some(subject)),
    }
}

use http::{Request, header::AUTHORIZATION};

use crate::{AcquisitionError, OnBehalfOf};

#[derive(Clone, Copy)]
pub(crate) enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
    UnexpectedSubject,
    Acquisition(AcquisitionError),
}

pub(crate) enum RequestAuth {
    None,
    OnBehalfOf(OnBehalfOf),
    AuthorizationConflict,
}

pub(crate) fn take_request_auth<B>(request: &mut Request<B>) -> RequestAuth {
    if request.headers().contains_key(AUTHORIZATION) {
        return RequestAuth::AuthorizationConflict;
    }
    request
        .extensions_mut()
        .remove::<OnBehalfOf>()
        .map_or(RequestAuth::None, RequestAuth::OnBehalfOf)
}

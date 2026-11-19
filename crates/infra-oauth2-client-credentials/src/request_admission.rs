use http::{Request, header::AUTHORIZATION};

use crate::{AcquisitionError, OnBehalfOf};

#[derive(Clone, Copy)]
pub(crate) enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
    UnexpectedSubject,
    Acquisition(AcquisitionError),
}

pub(crate) enum RequestSubject {
    None,
    OnBehalfOf(OnBehalfOf),
    AuthorizationConflict,
}

pub(crate) fn take_request_subject<B>(request: &mut Request<B>) -> RequestSubject {
    if request.headers().contains_key(AUTHORIZATION) {
        return RequestSubject::AuthorizationConflict;
    }
    request
        .extensions_mut()
        .remove::<OnBehalfOf>()
        .map_or(RequestSubject::None, RequestSubject::OnBehalfOf)
}

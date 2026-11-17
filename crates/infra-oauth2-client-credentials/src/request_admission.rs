use http::{Request, header::AUTHORIZATION};

use crate::{AcquisitionError, OnBehalfOf};

#[derive(Clone, Copy)]
pub(crate) enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
    UnexpectedSubject,
    Acquisition(AcquisitionError),
}

pub(crate) fn take_request_subject<B>(
    request: &mut Request<B>,
) -> Result<Option<OnBehalfOf>, AdmissionRefusal> {
    if request.headers().contains_key(AUTHORIZATION) {
        return Err(AdmissionRefusal::AuthorizationConflict);
    }
    Ok(request.extensions_mut().remove::<OnBehalfOf>())
}

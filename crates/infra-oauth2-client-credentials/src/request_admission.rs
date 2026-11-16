use http::{Request, header::AUTHORIZATION};

use crate::OnBehalfOf;

#[derive(Clone, Copy)]
pub(crate) enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
}

pub(crate) fn admit_request_subject<B>(
    request: &mut Request<B>,
    required: bool,
) -> Result<Option<OnBehalfOf>, AdmissionRefusal> {
    let subject = request.extensions_mut().remove::<OnBehalfOf>();
    if request.headers().contains_key(AUTHORIZATION) {
        return Err(AdmissionRefusal::AuthorizationConflict);
    }
    if required && subject.is_none() {
        return Err(AdmissionRefusal::SubjectRequired);
    }
    Ok(subject)
}

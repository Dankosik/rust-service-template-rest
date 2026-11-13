use http::{Request, header::AUTHORIZATION};

use crate::OnBehalfOf;

/// Both refusals are composition mistakes of this service.
#[derive(Clone, Copy)]
pub(crate) enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
}

/// Strips the subject extension even when admission fails, so it never
/// reaches the resource.
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

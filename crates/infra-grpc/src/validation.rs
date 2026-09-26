use std::sync::Arc;

use prost_protovalidate::{Error as ValidationError, Validator, ValidatorOption};
use prost_reflect::{DynamicMessage, ReflectMessage};
use service_failure::{ClassifiedFailure, Code};
use tonic::Status;

use crate::Error;
use crate::registration::ServiceDescriptor;
use crate::status::{ValidationDetail, classified_validation_status};

/// One immutable, preloaded validator owned by a generated service registry.
/// The runtime never accepts an unknown descriptor or lazily compiles a rule.
pub(crate) struct Validation {
    validator: Validator,
}

impl Validation {
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            validator: Validator::with_options(&[ValidatorOption::DisableLazy]),
        })
    }

    pub(crate) fn compile(descriptor: ServiceDescriptor) -> Result<Arc<Self>, Error> {
        if descriptor.descriptor_set().is_empty() {
            return Err(Error::InvalidValidation);
        }
        let descriptors = descriptor
            .methods()
            .iter()
            .map(|method| method.request_descriptor())
            .collect::<Vec<_>>();
        let validator = Validator::with_options(&[
            ValidatorOption::DisableLazy,
            ValidatorOption::AdditionalDescriptorSetBytes(descriptor.descriptor_set().to_vec()),
            ValidatorOption::MessageDescriptors(descriptors.clone()),
        ]);

        for message in descriptors {
            let empty = DynamicMessage::new(message);
            match validator.validate(&empty) {
                Ok(()) | Err(ValidationError::Validation(_)) => {}
                Err(ValidationError::Compilation(_) | ValidationError::Runtime(_)) => {
                    return Err(Error::InvalidValidation);
                }
                Err(_) => return Err(Error::InvalidValidation),
            }
        }
        Ok(Arc::new(Self { validator }))
    }

    pub(crate) fn validate<M: ReflectMessage>(&self, message: &M) -> Result<(), Status> {
        match self.validator.validate(message) {
            Ok(()) => Ok(()),
            Err(ValidationError::Validation(error)) => {
                let details = error
                    .violations()
                    .iter()
                    .take(32)
                    .map(|violation| {
                        ValidationDetail::new(
                            violation.field_path(),
                            violation.rule_id().to_owned(),
                        )
                    })
                    .collect();
                Err(classified_validation_status(
                    ClassifiedFailure::new(Code::UnprocessableContent),
                    details,
                ))
            }
            Err(ValidationError::Compilation(_) | ValidationError::Runtime(_)) => {
                Err(Status::internal(service_failure::SANITIZED_DETAIL))
            }
            Err(_) => Err(Status::internal(service_failure::SANITIZED_DETAIL)),
        }
    }
}

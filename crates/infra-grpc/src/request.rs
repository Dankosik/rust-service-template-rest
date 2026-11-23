use operation_context::OperationContext;

use crate::status::internal_status;

#[derive(Clone, Debug)]
pub struct ResponseContext(pub(crate) OperationContext);

impl ResponseContext {
    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        &self.0
    }
}

mod sealed {
    pub trait Sealed {}
    impl<T> Sealed for tonic::Request<T> {}
}

pub trait RequestExt: sealed::Sealed {
    fn operation(&self) -> Result<&OperationContext, tonic::Status>;

    fn response_context(&self) -> Result<&ResponseContext, tonic::Status>;

    // template:begin authn:grpc-request-ext-authn
    fn principal(&self) -> Result<&crate::Principal, tonic::Status>;

    fn delegation_credential(&self) -> Result<&crate::DelegationCredential, tonic::Status>;
    // template:end authn:grpc-request-ext-authn

    // template:begin service-auth:grpc-request-ext-service-auth
    fn caller(&self) -> Result<&crate::Caller, tonic::Status>;
    // template:end service-auth:grpc-request-ext-service-auth
}

impl<T> RequestExt for tonic::Request<T> {
    fn operation(&self) -> Result<&OperationContext, tonic::Status> {
        self.extensions().get().ok_or_else(internal_status)
    }

    fn response_context(&self) -> Result<&ResponseContext, tonic::Status> {
        self.extensions().get().ok_or_else(internal_status)
    }

    // template:begin authn:grpc-request-ext-authn-impl
    fn principal(&self) -> Result<&crate::Principal, tonic::Status> {
        self.extensions().get().ok_or_else(internal_status)
    }

    fn delegation_credential(&self) -> Result<&crate::DelegationCredential, tonic::Status> {
        self.extensions().get().ok_or_else(internal_status)
    }
    // template:end authn:grpc-request-ext-authn-impl

    // template:begin service-auth:grpc-request-ext-service-auth-impl
    fn caller(&self) -> Result<&crate::Caller, tonic::Status> {
        self.extensions().get().ok_or_else(internal_status)
    }
    // template:end service-auth:grpc-request-ext-service-auth-impl
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_extensions_return_a_sanitized_internal_status() {
        let mut request = tonic::Request::new(());
        request
            .extensions_mut()
            .insert(ResponseContext(OperationContext::unbounded()));
        let status = request.operation().unwrap_err();
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), service_failure::SANITIZED_DETAIL);
        assert_eq!(
            crate::status::catalog_code(&status),
            Some(service_failure::Code::InternalServerError)
        );
        assert!(request.response_context().is_ok());
        // template:begin authn:grpc-request-ext-authn-test
        for status in [
            request.principal().unwrap_err(),
            request.delegation_credential().unwrap_err(),
        ] {
            assert_eq!(status.code(), tonic::Code::Internal);
            assert_eq!(status.message(), service_failure::SANITIZED_DETAIL);
            assert_eq!(
                crate::status::catalog_code(&status),
                Some(service_failure::Code::InternalServerError)
            );
        }
        // template:end authn:grpc-request-ext-authn-test
    }
}

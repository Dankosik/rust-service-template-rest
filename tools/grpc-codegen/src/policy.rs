use std::{collections::BTreeSet, fmt::Write as _};

use heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use prost_build::{Method, Service, ServiceGenerator};
use prost_types::{DescriptorProto, FileDescriptorSet};

const MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

pub(crate) fn configure(config: &mut prost_build::Config, descriptor_set: &FileDescriptorSet) {
    for file in &descriptor_set.file {
        let package = file.package();
        for message in &file.message_type {
            configure_reflection_attributes(config, package, "", message);
        }
    }

    let server = tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .generate_default_stubs(false)
        .codec_path("::infra_grpc::generated::ValidatedCodec")
        .service_generator();
    let client = tonic_prost_build::configure()
        .build_client(true)
        .build_server(false)
        .codec_path("::infra_grpc::generated::BoundedClientCodec")
        .service_generator();

    config.service_generator(Box::new(CompositeGenerator {
        server,
        client,
        policy: PolicyGenerator,
    }));
}

struct CompositeGenerator {
    server: Box<dyn ServiceGenerator>,
    client: Box<dyn ServiceGenerator>,
    policy: PolicyGenerator,
}

impl ServiceGenerator for CompositeGenerator {
    fn generate(&mut self, service: Service, buffer: &mut String) {
        self.server.generate(service.clone(), buffer);
        self.client.generate(service.clone(), buffer);
        self.policy.generate(service, buffer);
    }

    fn finalize(&mut self, buffer: &mut String) {
        self.server.finalize(buffer);
        self.client.finalize(buffer);
        self.policy.finalize(buffer);
    }
}

struct PolicyGenerator;

impl ServiceGenerator for PolicyGenerator {
    fn generate(&mut self, service: Service, buffer: &mut String) {
        let service_stem = service.name.to_snake_case();
        let server_module = format!("{service_stem}_server");
        let service_constant = service.name.to_shouty_snake_case();
        let register_fn = format!("register_{service_stem}");
        let client_transport_fn = format!("{service_stem}_client_transport");
        let input_descriptors = service
            .methods
            .iter()
            .map(|method| {
                (
                    method.input_type.clone(),
                    format!(
                        "{}_descriptor",
                        method
                            .input_type
                            .rsplit("::")
                            .next()
                            .expect("prost generated input type")
                            .to_snake_case()
                    ),
                )
            })
            .collect::<BTreeSet<_>>();

        for (input_type, descriptor_fn) in &input_descriptors {
            writeln!(buffer, "#[doc(hidden)]").expect("writing generated code");
            writeln!(
                buffer,
                "fn {descriptor_fn}() -> ::prost_reflect::MessageDescriptor {{\n    \
                 <{input_type} as ::prost_reflect::ReflectMessage>::descriptor(&{input_type}::default())\n}}"
            )
            .expect("writing generated code");
        }

        writeln!(buffer, "\n#[doc(hidden)]").expect("writing generated code");
        writeln!(
            buffer,
            "const {service_constant}_METHODS: [::infra_grpc::generated::Method; {}] = [",
            service.methods.len()
        )
        .expect("writing generated code");
        for method in &service.methods {
            writeln!(
                buffer,
                "    ::infra_grpc::generated::Method::new(\"{}\", \"{}\", \"{}\", {}, {}),",
                method_path(&service, method),
                method.input_proto_type.trim_start_matches('.'),
                method.output_proto_type.trim_start_matches('.'),
                cardinality(method),
                descriptor_fn_for(method),
            )
            .expect("writing generated code");
        }
        writeln!(buffer, "];").expect("writing generated code");
        writeln!(buffer, "#[doc(hidden)]").expect("writing generated code");
        writeln!(
            buffer,
            "const {service_constant}_SERVICE: ::infra_grpc::generated::ServiceDescriptor = \
             ::infra_grpc::generated::ServiceDescriptor::new(\"{}.{}\", &{service_constant}_METHODS, \
             crate::DESCRIPTOR_BYTES);",
            service.package, service.proto_name,
        )
        .expect("writing generated code");

        writeln!(buffer, "#[doc(hidden)]").expect("writing generated code");
        writeln!(
            buffer,
            "pub fn {register_fn}<T>(services: &mut ::infra_grpc::Services, implementation: T) -> \
             std::result::Result<(), ::infra_grpc::Error>\nwhere\n    T: {server_module}::{},\n{{",
            service.name,
        )
        .expect("writing generated code");
        writeln!(
            buffer,
            "    let server = {server_module}::{}Server::new(\n        \
             ::infra_grpc::generated::GovernedService::new(implementation),\n    )\n    \
             .max_decoding_message_size({MAX_MESSAGE_SIZE})\n    \
             .max_encoding_message_size({MAX_MESSAGE_SIZE});",
            service.name,
        )
        .expect("writing generated code");
        writeln!(
            buffer,
            "    services.register(server, {service_constant}_SERVICE)\n}}"
        )
        .expect("writing generated code");

        writeln!(buffer, "#[doc(hidden)]").expect("writing generated code");
        writeln!(
            buffer,
            "pub fn {client_transport_fn}(client: ::infra_grpc::Client) -> \
             std::result::Result<::infra_grpc::Client, ::infra_grpc::Error> {{\n    \
             client.with_service({service_constant}_SERVICE)\n}}"
        )
        .expect("writing generated code");

        writeln!(buffer, "#[tonic::async_trait]").expect("writing generated code");
        writeln!(
            buffer,
            "impl<T> {server_module}::{} for ::infra_grpc::generated::GovernedService<T>\nwhere\n    \
             T: {server_module}::{},\n{{",
            service.name, service.name,
        )
        .expect("writing generated code");

        for method in &service.methods {
            write_proxy_method(buffer, method);
        }

        writeln!(buffer, "}}").expect("writing generated code");
    }
}

fn configure_reflection_attributes(
    config: &mut prost_build::Config,
    package: &str,
    enclosing: &str,
    message: &DescriptorProto,
) {
    let name = message.name();
    assert!(!name.is_empty(), "valid Buf descriptors name every message");
    let full_name = match (package.is_empty(), enclosing.is_empty()) {
        (true, true) => name.to_owned(),
        (true, false) => format!("{enclosing}.{name}"),
        (false, true) => format!("{package}.{name}"),
        (false, false) => format!("{package}.{enclosing}.{name}"),
    };

    let matcher = format!(".{full_name}");
    config.message_attribute(&matcher, "#[derive(::prost_reflect::ReflectMessage)]");
    config.message_attribute(
        &matcher,
        format!(
            "#[prost_reflect(descriptor_pool = \"crate::DESCRIPTOR_POOL\", message_name = \"{full_name}\")]"
        ),
    );

    let nested_enclosing = match enclosing.is_empty() {
        true => name.to_owned(),
        false => format!("{enclosing}.{name}"),
    };
    for nested in &message.nested_type {
        configure_reflection_attributes(config, package, &nested_enclosing, nested);
    }
}

fn write_proxy_method(buffer: &mut String, method: &Method) {
    let input = &method.input_type;
    let output = &method.output_type;
    let method_name = &method.name;

    match (method.client_streaming, method.server_streaming) {
        (false, false) => {
            writeln!(
                buffer,
                "    async fn {method_name}(&self, request: tonic::Request<{input}>) -> \
                 std::result::Result<tonic::Response<{output}>, tonic::Status> {{\n        \
                 ::infra_grpc::generated::guard_unary(self.inner().{method_name}(request)).await\n    }}"
            )
            .expect("writing generated code");
        }
        (true, false) => {
            writeln!(
                buffer,
                "    async fn {method_name}(&self, request: tonic::Request<tonic::Streaming<{input}>>) -> \
                 std::result::Result<tonic::Response<{output}>, tonic::Status> {{\n        \
                 ::infra_grpc::generated::guard_unary(self.inner().{method_name}(request)).await\n    }}"
            )
            .expect("writing generated code");
        }
        (false, true) | (true, true) => {
            let associated_stream = format!("{}Stream", method.name.to_upper_camel_case());
            let request = if method.client_streaming {
                format!("tonic::Streaming<{input}>")
            } else {
                input.clone()
            };
            writeln!(
                buffer,
                "    type {associated_stream} = ::infra_grpc::generated::GuardedStream<T::{associated_stream}>;"
            )
            .expect("writing generated code");
            writeln!(
                buffer,
                "    async fn {method_name}(&self, request: tonic::Request<{request}>) -> \
                 std::result::Result<tonic::Response<Self::{associated_stream}>, tonic::Status> {{\n        \
                 ::infra_grpc::generated::guard_stream(self.inner().{method_name}(request)).await\n    }}"
            )
            .expect("writing generated code");
        }
    }
}

fn method_path(service: &Service, method: &Method) -> String {
    format!(
        "/{}.{}/{}",
        service.package, service.proto_name, method.proto_name
    )
}

fn cardinality(method: &Method) -> &'static str {
    match (method.client_streaming, method.server_streaming) {
        (false, false) => "::infra_grpc::generated::Cardinality::Unary",
        (true, false) => "::infra_grpc::generated::Cardinality::ClientStreaming",
        (false, true) => "::infra_grpc::generated::Cardinality::ServerStreaming",
        (true, true) => "::infra_grpc::generated::Cardinality::BidiStreaming",
    }
}

fn descriptor_fn_for(method: &Method) -> String {
    format!(
        "{}_descriptor",
        method
            .input_type
            .rsplit("::")
            .next()
            .expect("prost generated input type")
            .to_snake_case()
    )
}

use proc_macro::TokenStream;
// Turn the Rust template code into tokens
use quote::{quote, quote_spanned};
// ItemFn represents the abstract syntax tree; LitStr is a string literal that keeps both the string content and its source location. Parser trait
use syn::{Attribute, FnArg, Ident, ItemFn, LitStr, Pat, Type, parse::Parser, spanned::Spanned};

// Holds the configuration read from #[tool(...)]
struct ToolConfig {
    // LitStr carries location information
    description: LitStr,

    // Optional register hook (= created): name of the companion function to run at registration time.
    // Signature must be `fn(&mut ToolContext) -> Result<(), ToolError>`.
    on_register: Option<Ident>,

    // Optional unregister hook (= destroy): name of the companion function to run at unregistration time.
    // Signature must be `fn(&mut ToolContext) -> Result<(), ToolError>`.
    on_unregister: Option<Ident>,
}

struct ToolParameter {
    // Parameter name
    name: Ident,

    // Parameter type
    ty: Type,

    // Parameter description
    description: LitStr,
}

// The actual arguments of one tool call, arranged in the original function parameter order.
// The context parameter does not go into the parameter schema, but it still has to be passed to the original function positionally, so the order is preserved here.
enum CallArgument {
    // An ordinary parameter deserialized from the JSON arguments
    Field(Ident),
    // The runtime context (injected by the SDK), see the SDK's ToolContext.
    // by_ref means the original function declared `&ToolContext`, otherwise it is a by-value `ToolContext`.
    Context { by_ref: bool },
}

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Handle the two cases separately based on the parse result
    match try_expand(attr, item) {
        Ok(code) => code.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn try_expand(attr: TokenStream, item: TokenStream) -> syn::Result<proc_macro2::TokenStream> {
    let config = parse_config(attr)?;

    // Parse the annotated code as a function; if not, error out directly
    let mut function = syn::parse::<ItemFn>(item)?;

    // Extract the parameter info, clearing the helper attributes already consumed
    let (parameters, call_arguments) = extract_parameters(&mut function)?;

    // Generate code from the collected info.
    // generate_tool returns code tokens, wrapped in Ok as a success result.
    Ok(generate_tool(function, config, parameters, call_arguments))
}

// Parse the contents inside the #tool[(...)] parentheses
//
// Supported keys:
// - `description = "..."` (required)
// - `on_register = fn_name` (optional, = created hook)
// - `on_unregister = fn_name` (optional, = destroy hook)
fn parse_config(attr: TokenStream) -> syn::Result<ToolConfig> {
    let mut description: Option<LitStr> = None;
    let mut on_register: Option<Ident> = None;
    let mut on_unregister: Option<Ident> = None;

    // Attribute parser: this closure is called for each comma-separated config item #tool(a=1,b=2,...)
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("on_register") {
            return read_hook(meta, &mut on_register, "on_register");
        }
        if meta.path.is_ident("on_unregister") {
            return read_hook(meta, &mut on_unregister, "on_unregister");
        }
        read_description(meta, &mut description)
    });

    parser.parse(attr)?;

    let description = description.ok_or_else(|| {
        // Raise a macro compile error when it is not filled in
        syn::Error::new(
            // Span::call_site indicates the call site
            proc_macro2::Span::call_site(),
            "missing description; write #tool(description = \"tool description\")",
        )
    })?;

    Ok(ToolConfig {
        description,
        on_register,
        on_unregister,
    })
}

// Read the value of `on_register` / `on_unregister`: a companion function name (Ident).
fn read_hook(
    meta: syn::meta::ParseNestedMeta<'_>,
    slot: &mut Option<Ident>,
    key: &str,
) -> syn::Result<()> {
    if slot.is_some() {
        return Err(meta.error(format!("{key} must not be repeated")));
    }
    let value: Ident = meta.value()?.parse()?;
    *slot = Some(value);
    Ok(())
}

// Parse #[param()]
fn parse_parameter_description(attribute: &Attribute) -> syn::Result<LitStr> {
    let mut description = None;
    // Parse the config items one by one
    attribute.parse_nested_meta(|meta| {
        // Reuse the validation written in the previous round
        // Check the name, duplicate entries, string type and empty string
        read_description(meta, &mut description)
    })?;

    // If #[param()] was written
    description.ok_or_else(|| syn::Error::new_spanned(attribute, "missing description"))
}

// Determine whether the context parameter is by reference (`&ToolContext`).
fn context_is_by_ref(ty: &Type) -> bool {
    matches!(ty, Type::Reference(_))
}

// Determine whether a type is the runtime context parameter (ToolContext).
//
// Match on the last segment of the type path, so both `ToolContext` and
// `shirley_agent_sdk::ToolContext` are recognized.
fn is_tool_context_type(ty: &Type) -> bool {
    // Both `&ToolContext` and `ToolContext` count
    let ty = match ty {
        Type::Reference(reference) => reference.elem.as_ref(),
        other => other,
    };
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "ToolContext")
}

// Read the parameters. Returns (parameters that go into the schema, actual call arguments in original order).
fn extract_parameters(function: &mut ItemFn) -> syn::Result<(Vec<ToolParameter>, Vec<CallArgument>)> {
    let mut parameters = vec![];
    let mut call_arguments = vec![];
    let mut has_context = false;

    // sig is the function signature, inputs is the parameter list
    for input in &mut function.sig.inputs {
    // self and ordinary parameters Typed location: String
        let FnArg::Typed(parameter) = input else {
            return Err(syn::Error::new_spanned(input, "tools do not support self methods"));
        };

    // Possibly a destructured tuple; only simple forms are supported for now
        let Pat::Ident(pattern) = parameter.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &parameter.pat,
                "tool parameters must use a simple name; destructuring is not supported",
            ));
        };

    // Exclude complex bindings such as `ref name` and `name @ pattern`
    // `mut name` may be kept; it does not affect parameter-name extraction
        if pattern.by_ref.is_some() || pattern.subpat.is_some() {
            return Err(syn::Error::new_spanned(
                pattern,
                "tool parameters do not support ref or @ bindings",
            ));
        }

        // Context parameter: does not go into the schema, passed to the original function positionally.
        if is_tool_context_type(&parameter.ty) {
            if has_context {
                return Err(syn::Error::new_spanned(
                    &parameter.ty,
                    "a tool accepts at most one ToolContext parameter",
                ));
            }
            if let Some(attribute) = parameter.attrs.first() {
                return Err(syn::Error::new_spanned(
                    attribute,
                    "ToolContext parameters must not carry #[param(...)]; \
                     the context is injected by the SDK, not by the model",
                ));
            }
            has_context = true;
            call_arguments.push(CallArgument::Context {
                by_ref: context_is_by_ref(&parameter.ty),
            });
            continue;
        }

        let mut description = None;

        for attribute in &parameter.attrs {
            if !attribute.path().is_ident("param") {
                return Err(syn::Error::new_spanned(
                    attribute,
                    "tool parameters only support the #[param(...)] attribute",
                ));
            }

            // Must not be repeated
            if description.is_some() {
                return Err(syn::Error::new_spanned(attribute, "#[param] must not be repeated"));
            }

            description = Some(parse_parameter_description(attribute)?);
        }

        let description = description.ok_or_else(|| {
            syn::Error::new_spanned(&parameter.pat, "missing #[param(description = \"parameter description\")]")
        })?;

        call_arguments.push(CallArgument::Field(pattern.ident.clone()));

        parameters.push(ToolParameter {
            name: pattern.ident.clone(),
            ty: (*parameter.ty).clone(),
            description,
        });

        // Clear the param helper attribute
        parameter.attrs.clear();
    }
    Ok((parameters, call_arguments))
}

// Generic reader for #[xxx(description = xxx)]
fn read_description(
    meta: syn::meta::ParseNestedMeta<'_>,
    description: &mut Option<LitStr>,
) -> syn::Result<()> {
    // path corresponds to the key
    if !meta.path.is_ident("description") {
        return Err(meta.error("unknown option; only description is supported"));
    }
    // Duplicate-key check
    if description.is_some() {
        return Err(meta.error("description must not be repeated"));
    }

    // value() consumes the =, parse() consumes the value
    let value: LitStr = meta.value()?.parse()?;

    // Check whether the value is empty
    if value.value().trim().is_empty() {
        return Err(syn::Error::new(value.span(), "description must not be empty"));
    }

    *description = Some(value);

    // Not returning an error means success
    Ok(())
}

fn generate_tool(
    function: ItemFn,
    config: ToolConfig,
    parameters: Vec<ToolParameter>,
    call_arguments: Vec<CallArgument>,
) -> proc_macro2::TokenStream {
    // Original function name
    let name = &function.sig.ident;

    // Function visibility pub / private
    let visibility = &function.vis;

    // Function description
    let description = &config.description;

    let fields = parameters.iter().map(|parameter| {
        let field_name = &parameter.name;
        let field_type = &parameter.ty;
        let field_description = &parameter.description;

        quote! {
            #[schemars(description = #field_description)]
            #field_name: #field_type
        }
    });

    let call_arguments = call_arguments.iter().map(|argument| match argument {
        CallArgument::Field(field_name) => quote! {
            args.#field_name
        },
        // Pass it in the form the original function declares: `&ToolContext` borrows, `ToolContext` moves by value.
        CallArgument::Context { by_ref } => {
            if *by_ref {
                quote! { &__tool_context }
            } else {
                quote! { __tool_context.clone() }
            }
        }
    });

    // Optional register / unregister hooks: generate `on_register` / `on_unregister` overrides that
    // directly call the user-written companion functions (same level as the tool function, hence `super::`).
    // If no hook is written, no override is generated -- `GenerateTool` inherits the trait's default empty impl.
    let on_register_impl = config.on_register.as_ref().map(|hook| {
        quote! {
            fn on_register(
                &mut self,
                ctx: &mut ::shirley_agent_sdk::ToolContext,
            ) -> ::std::result::Result<(), ::shirley_agent_sdk::ToolError> {
                super::#hook(ctx)
            }
        }
    });

    let on_unregister_impl = config.on_unregister.as_ref().map(|hook| {
        quote! {
            fn on_unregister(
                &mut self,
                ctx: &mut ::shirley_agent_sdk::ToolContext,
            ) -> ::std::result::Result<(), ::shirley_agent_sdk::ToolError> {
                super::#hook(ctx)
            }
        }
    });

    // Get the code location of the return type
    let return_span = function.sig.output.span();
    // `_` lets the compiler infer the success value type; the error type is fixed to the SDK's ToolError
    let invoke_function = quote_spanned! {return_span=>
        let outcome: ::std::result::Result<_, ::shirley_agent_sdk::ToolError> = super::#name(
            #(#call_arguments),*
        ).await;
        let result = outcome?;
    };

    quote! {
        #function

        #visibility mod #name {
            use super::*;

            // Automatically implement argument parsing and schema generation
            #[derive(::serde::Deserialize, ::schemars::JsonSchema)]
            #[serde(deny_unknown_fields)]
            // Reject unknown fields when parsing arguments
            // The schema correspondingly forbids additional properties
            struct Arguments {
                // Insert all fields, adding a trailing comma after each field
                #(#fields,)*
            }


            // Should the struct be capitalized here?
            struct GenerateTool {
                definition: ::shirley_agent_sdk::ToolDefinition,
            }

            impl ::shirley_agent_sdk::Tool for GenerateTool {
                fn definition(&self) -> &::shirley_agent_sdk::ToolDefinition {
                    &self.definition
                }

                #on_register_impl
                #on_unregister_impl

                // Accept the unified JSON arguments and runtime context passed in by the SDK
                fn invoke(
                    &self,
                    input: ::serde_json::Value,
                    __tool_context: ::shirley_agent_sdk::ToolContext,
                ) -> ::shirley_agent_sdk::ToolFuture<'_> {
                    // 1. Deserialize the input

                    // 2. Call the original function
                    Box::pin(async move {
                        let args = ::serde_json::from_value::<Arguments>(input)
                            .map_err(|error| {
                                ::shirley_agent_sdk::ToolError::ArgumentsError(
                                    ::std::format!("invalid tool arguments: {error}")
                                )
                        })?;

                        // Call the function, passing the arguments
                        #invoke_function

                        // Return the serialized JSON result
                        ::serde_json::to_value(result)
                            .map_err(|error| {
                                ::shirley_agent_sdk::ToolError::ExecutionError(
                                    ::std::format!("failed to serialize tool result: {error}")
                                )
                            })

                    })
                }
            }

            pub fn definition() -> ::shirley_agent_sdk::ToolDefinition {
                ::shirley_agent_sdk::ToolDefinition {
                    name: ::std::string::String::from(
                        stringify!(#name)
                    ),

                    description: ::std::string::String::from(
                        #description
                    ),

                    parameters: ::serde_json::json!(
                        ::schemars::schema_for!(Arguments)
                    ),
                }
            }

            pub fn tool() -> impl ::shirley_agent_sdk::Tool + 'static {
                GenerateTool {
                    definition: definition()
                }
            }
        }
    }
}

use proc_macro::TokenStream;
// 把rust模版代码转换成token
use quote::{quote, quote_spanned};
// ItemFn 用来表示抽象语法树, LitStr 字符串字面量，既保留字符串内容，也保留源码位置。 Parser trait
use syn::{Attribute, FnArg, Ident, ItemFn, LitStr, Pat, Type, parse::Parser, spanned::Spanned};

// 保存从 #[tool(...)] 中读取的配置
struct ToolConfig {
    // LisStr 带有位置信息
    description: LitStr,
}

struct ToolParameter {
    // 参数名
    name: Ident,

    // 参数类型
    ty: Type,

    // 参数描述
    description: LitStr,
}

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    // 根据解析结果分别处理
    match try_expand(attr, item) {
        Ok(code) => code.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn try_expand(attr: TokenStream, item: TokenStream) -> syn::Result<proc_macro2::TokenStream> {
    let config = parse_config(attr)?;

    // 注释修饰的代码解析成函数，如果不是直接报错
    let mut function = syn::parse::<ItemFn>(item)?;

    // 提取参数信息，同时清除已经消费的辅助注解
    let parameters = extract_parameters(&mut function)?;

    // 根据收集的信息生成代码。
    // generate_tool 返回代码 token，用 Ok 包装成成功结果。
    Ok(generate_tool(function, config, parameters))
}

// 解析 #tool[(...)] 括号里面的内容
fn parse_config(attr: TokenStream) -> syn::Result<ToolConfig> {
    let mut description: Option<LitStr> = None;

    // 属性解析器，目前只支持 description
    // 它会对每个逗号分隔的配置项调用这个闭包 #tool(a=1,b=2,...)
    let parser = syn::meta::parser(|meta| read_description(meta, &mut description));

    parser.parse(attr)?;

    let description = description.ok_or_else(|| {
        //  没有填写抛出宏编译错误
        syn::Error::new(
            // Span::call_site 表示被调用的位置
            proc_macro2::Span::call_site(),
            "missing description; write #tool(description = \"tool description\")",
        )
    })?;

    Ok(ToolConfig { description })
}

// 解析 #[param()]
fn parse_parameter_description(attribute: &Attribute) -> syn::Result<LitStr> {
    let mut description = None;
    // 足个解析阔好李敏啊的配置
    attribute.parse_nested_meta(|meta| {
        // 复用上一轮写好的校验
        // 检查名称，重复填写，字符串类型和空字符串
        read_description(meta, &mut description)
    })?;

    // 如果写的 #[param()]
    description.ok_or_else(|| syn::Error::new_spanned(attribute, "missing description"))
}

// 读取参数
fn extract_parameters(function: &mut ItemFn) -> syn::Result<Vec<ToolParameter>> {
    let mut parameters = vec![];

    // sig 是函数签名，inputs 是参数列表
    for input in &mut function.sig.inputs {
        // self 和 普通参数 Typed location: String
        let FnArg::Typed(parameter) = input else {
            return Err(syn::Error::new_spanned(input, "tools do not support self methods"));
        };

        // 可能是解构的元组, 目前只支持简单的
        let Pat::Ident(pattern) = parameter.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &parameter.pat,
                "tool parameters must use a simple name; destructuring is not supported",
            ));
        };

        // 排除 ref name 和 name @ pattern 这类复杂绑定
        // mut name 可以保留，不影响参观名称提取
        if pattern.by_ref.is_some() || pattern.subpat.is_some() {
            return Err(syn::Error::new_spanned(
                pattern,
                "tool parameters do not support ref or @ bindings",
            ));
        }

        let mut description = None;

        for attribute in &parameter.attrs {
            if !attribute.path().is_ident("param") {
                return Err(syn::Error::new_spanned(
                    attribute,
                    "tool parameters only support the #[param(...)] attribute",
                ));
            }

            // 不能重复写
            if description.is_some() {
                return Err(syn::Error::new_spanned(attribute, "#[param] must not be repeated"));
            }

            description = Some(parse_parameter_description(attribute)?);
        }

        let description = description.ok_or_else(|| {
            syn::Error::new_spanned(&parameter.pat, "missing #[param(description = \"parameter description\")]")
        })?;

        parameters.push(ToolParameter {
            name: pattern.ident.clone(),
            ty: (*parameter.ty).clone(),
            description,
        });

        // 清除 param 辅助注解
        parameter.attrs.clear();
    }
    Ok(parameters)
}

// 通用读取#[xxx(description = xxx)]
fn read_description(
    meta: syn::meta::ParseNestedMeta<'_>,
    description: &mut Option<LitStr>,
) -> syn::Result<()> {
    // path 相当于 key
    if !meta.path.is_ident("description") {
        return Err(meta.error("unknown option; only description is supported"));
    }
    // 重复key判断
    if description.is_some() {
        return Err(meta.error("description must not be repeated"));
    }

    // value() 消费 =，parse() 消费 value
    let value: LitStr = meta.value()?.parse()?;

    // 检查值是否为空
    if value.value().trim().is_empty() {
        return Err(syn::Error::new(value.span(), "description must not be empty"));
    }

    *description = Some(value);

    // 不返回错误说明成功了
    Ok(())
}

fn generate_tool(
    function: ItemFn,
    config: ToolConfig,
    parameters: Vec<ToolParameter>,
) -> proc_macro2::TokenStream {
    // 原始函数名
    let name = &function.sig.ident;

    // 函数可见性 pub / private
    let visibility = &function.vis;

    // 函数描述
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

    let call_arguments = parameters.iter().map(|parameter| {
        let field_name = &parameter.name;

        quote! {
            args.#field_name
        }
    });

    // 获取返回值类型的代码位置
    let return_span = function.sig.output.span();
    // _ 让编译器推导成功值类型，错误类型固定为 SDK 的 ToolError
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

            // 自动实现参数解析和 Schema 生成能力
            #[derive(::serde::Deserialize, ::schemars::JsonSchema)]
            #[serde(deny_unknown_fields)]
            // 解析参数时拒绝未知字段
            // Schema 中也会相应禁止额外属性
            struct Arguments {
                // 重复插入所有字段，没个字段后面添加逗号
                #(#fields,)*
            }


            // 这里应该大写结构体？
            struct GenerateTool {
                definition: ::shirley_agent_sdk::ToolDefinition,
            }

            impl ::shirley_agent_sdk::Tool for GenerateTool {
                fn definition(&self) -> &::shirley_agent_sdk::ToolDefinition {
                    &self.definition
                }

                // 接受 SDK 统一传入的 JSON 参数
                fn invoke(&self, input: ::serde_json::Value) -> ::shirley_agent_sdk::ToolFuture<'_> {
                    // 1. 将 input 反序列化

                    // 2. 调用原始函数
                    Box::pin(async move {
                        let args = ::serde_json::from_value::<Arguments>(input)
                            .map_err(|error| {
                                ::shirley_agent_sdk::ToolError::ArgumentsError(
                                    ::std::format!("invalid tool arguments: {error}")
                                )
                        })?;

                        // 调用函数，并传入参数
                        #invoke_function

                        // 返回序列话的json结果
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

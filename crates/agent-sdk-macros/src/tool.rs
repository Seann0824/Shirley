use proc_macro::TokenStream;
// 把rust模版代码转换成token
use quote::quote;
// ItemFn 用来表示抽象语法树, LitStr 字符串字面量，既保留字符串内容，也保留源码位置。 Parser trait
use syn::{ItemFn, LitStr, parse::Parser};

// 保存从 #[tool(...)] 中读取的配置
struct ToolConfig {
    // LisStr 带有位置信息
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
    let function = syn::parse::<ItemFn>(item)?;

    // 获取 LisStr 的字符串
    let _description = config.description.value();

    Ok(quote! {
        #function
    })
}

// 解析 #tool[(...)] 括号里面的内容
fn parse_config(attr: TokenStream) -> syn::Result<ToolConfig> {
    let mut description: Option<LitStr> = None;

    // 属性解析器，目前只支持 description
    // 它会对每个逗号分隔的配置项调用这个闭包 #tool(a=1,b=2,...)
    let parser = syn::meta::parser(|meta| {
        // path 相当于 key
        if !meta.path.is_ident("description") {
            return Err(meta.error("未知配置项，只支持 description"));
        }
        // 重复key判断
        if description.is_some() {
            return Err(meta.error("description 不能重复填写"));
        }

        // value() 消费 =，parse() 消费 value
        let value: LitStr = meta.value()?.parse()?;

        // 检查值是否为空
        if value.value().trim().is_empty() {
            return Err(syn::Error::new(value.span(), "description 不能为空"));
        }

        description = Some(value);

        // 不返回错误说明成功了
        Ok(())
    });

    parser.parse(attr)?;

    let description = description.ok_or_else(|| {
        //  没有填写抛出宏编译错误
        syn::Error::new(
            // Span::call_site 表示被调用的位置
            proc_macro2::Span::call_site(),
            "缺少 description, 请填写 #tool(description = \"工具说明\")",
        )
    })?;

    Ok(ToolConfig { description })
}

use proc_macro::TokenStream;
// 把rust模版代码转换成token
use quote::quote;
// 用来表示抽象语法树
use syn::ItemFn;

pub(crate) fn expand(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // 1. rust代码块解析成语法树
    let parsed = syn::parse::<ItemFn>(item);

    // 根据解析结果分别处理
    match parsed {
        Ok(function) => {
            // 生成 Rust 代码 Token
            // #function 是 quote! 的插值与法： 把 function 语法树对应的代码插入到这里
            let generated = quote! {
                #function
            };

            // quote! 返回 proc_macro2::TokenStream
            generated.into()
        }
        Err(error) => {
            // 把错误转换成 compile_error!() 代码
            let generated = error.to_compile_error();

            generated.into()
        }
    }
}

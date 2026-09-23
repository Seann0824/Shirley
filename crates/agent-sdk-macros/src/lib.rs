use proc_macro::TokenStream;

mod tool;

#[proc_macro_attribute]
pub fn tool(
    // 宏定义的参数
    attr: TokenStream,
    // 修饰的代码块
    item: TokenStream,
) -> TokenStream {
    // 根据获得的原始代码，去生成新的代码块冰返回。

    tool::expand(attr, item)
}

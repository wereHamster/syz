use crate::core::engine::PackageInfo;

pub fn get_group(package_info: &PackageInfo) -> Option<String> {
    if let Some(url) = &package_info.repo_url {
        if url.contains("github.com/Dwlad90/stylex-swc-plugin") {
            return Some("StyleX SWC".to_string());
        }
        if url.contains("github.com/tailwindlabs/tailwindcss") {
            return Some("Tailwind CSS".to_string());
        }
        if url.contains("github.com/lingui/js-lingui") {
            return Some("Lingui".to_string());
        }
        if url.contains("github.com/dotansimha/graphql-code-generator")
            || url.contains("github.com/dotansimha/graphql-codegen")
            || url.contains("github.com/dotansimha/graphql-code-generator-community")
        {
            return Some("GraphQL Code Generator".to_string());
        }
        if url.contains("github.com/getsentry/sentry-javascript") {
            return Some("Sentry (JavaScript)".to_string());
        }
        if url.contains("github.com/axe312ger/sqip") {
            return Some("SQIP".to_string());
        }
        if url.contains("github.com/facebook/react") {
            return Some("React".to_string());
        }
        if url.contains("github.com/nrwl/nx") {
            return Some("Nx".to_string());
        }
        if url.contains("github.com/timvir/timvir") {
            return Some("Timvir".to_string());
        }
        if url.contains("github.com/vercel/next.js") {
            return Some("Next.js".to_string());
        }
        if url.contains("github.com/babel/babel") {
            return Some("Babel".to_string());
        }
        if url.contains("github.com/visgl/deck.gl") {
            return Some("deck.gl".to_string());
        }
        if url.contains("github.com/mui/material-ui") {
            return Some("Material UI".to_string());
        }
        if url.contains("github.com/microsoft/playwright") {
            return Some("Playwright".to_string());
        }
        if url.contains("github.com/formatjs/formatjs") {
            return Some("FormatJS".to_string());
        }
        if url.contains("github.com/airbnb/visx") {
            return Some("visx".to_string());
        }
    }
    None
}

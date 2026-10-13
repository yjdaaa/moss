//! NV11 opt-in information plugins. Providers are deliberately disabled until individually wired and verified.
pub mod http;
pub mod weather;
pub mod search;
pub mod metaso;
pub mod rag;
pub mod lunar;
pub mod news;
pub mod newsnow;
pub mod cache;
pub mod runtime;

#[cfg(test)]
mod contract_tests;

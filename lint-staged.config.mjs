export default {
  // Rust：rustfmt 会就近读取 Cargo.toml 以确定 edition
  "*.rs": "rustfmt",
  // Node：交给 prettier，--ignore-unknown 保证不认识的文件不报错
  "*.{js,jsx,ts,tsx,mjs,cjs,json,jsonc,css,scss,md,yml,yaml,html}":
    "prettier --write --ignore-unknown",
};

export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    // 关闭 subject 大小写校验，保证中文 / 大写缩写开头的标题可用
    "subject-case": [0],
    // 放宽正文行宽，避免中文长行被误报
    "body-max-line-length": [0],
    // 页脚同理
    "footer-max-line-length": [0],
    // 标题总长放宽到 100
    "header-max-length": [2, "always", 100],
  },
};

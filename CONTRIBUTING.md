# 贡献指南

## 仓库布局

```
lxlake/
├── crates/          # Rust 成员 crate
├── packages/        # Node 包（pnpm workspace 成员）
├── .husky/          # git hooks
├── .editorconfig
├── .gitattributes
├── .gitignore
├── .gitmessage
└── pnpm-workspace.yaml
```

## 环境要求

| 工具    | 版本                            |
| ------- | ------------------------------- |
| Node.js | >= 22                           |
| pnpm    | 12.x（`packageManager` 已锁定） |
| Rust    | stable，含 `rustfmt`            |

首次克隆后执行一次 `pnpm install`，会自动注册 git hooks。

## 分支策略

- `main` 为唯一长期分支，始终保持可发布状态
- 功能开发使用短生命周期分支，命名 `feat/<简短描述>`、`fix/<简短描述>`
- 通过 Pull Request 合并到 `main`，不直接向 `main` 推送

## 提交规范

采用 [Conventional Commits](https://www.conventionalcommits.org/)，格式：

```
<type>(<scope>): <subject>
```

### type

| type       | 含义                       |
| ---------- | -------------------------- |
| `feat`     | 新功能                     |
| `fix`      | 缺陷修复                   |
| `docs`     | 仅文档变更                 |
| `style`    | 不影响逻辑的格式调整       |
| `refactor` | 重构（不修缺陷也不加功能） |
| `perf`     | 性能优化                   |
| `test`     | 新增或修改测试             |
| `build`    | 构建系统或依赖变更         |
| `ci`       | CI 配置与脚本变更          |
| `chore`    | 其它杂项                   |
| `revert`   | 回滚某次提交               |

### 约定

- `scope` 可选，填写受影响模块
- `subject` 使用祈使句，不加结尾句号，中文英文均可，建议 50 字符内
- 完整标题不超过 100 字符
- 破坏性变更在页脚写 `BREAKING CHANGE: <说明与迁移方式>`

### 示例

```
feat(cli): 支持从环境变量读取对象存储凭证

fix(core): 修正分片上传在并发重试时的计数错误

docs: 补充本地开发启动步骤

BREAKING CHANGE: 配置项 storage.endpoint 更名为 storage.uri
```

`git commit` 时会自动加载 [.gitmessage](.gitmessage) 作为模板，按提示填写即可。

## git hooks

由 husky 管理，`pnpm install` 时自动安装：

| 钩子         | 行为                                               |
| ------------ | -------------------------------------------------- |
| `commit-msg` | 用 commitlint 校验提交信息是否符合上面的规范       |
| `pre-commit` | 用 lint-staged 对暂存文件跑 `rustfmt` / `prettier` |

校验不通过时提交会被拒绝，按提示修正后重新提交。不建议使用 `--no-verify` 绕过钩子。

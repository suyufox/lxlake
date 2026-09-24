# 贡献指南

## 仓库布局

```
lxlake/
├── crates/          # Rust 成员 crate
├── packages/        # Node 包（pnpm workspace 成员）
├── docs/            # 设计与规划文档
├── .husky/          # git hooks
├── .editorconfig
├── .gitattributes
├── .gitignore
├── .gitmessage
├── CONTRIBUTING.md
└── pnpm-workspace.yaml
```

## 文档

| 文档                                         | 内容                                                             |
| -------------------------------------------- | ---------------------------------------------------------------- |
| [docs/architecture.md](docs/architecture.md) | 定位、平台范围、分层、分包与拆包判据、feature 分档、webview 双线 |
| [docs/roadmap.md](docs/roadmap.md)           | 里程碑、当前目标与验收、之后要做的                               |
| [docs/environment.md](docs/environment.md)   | 工具链基线、环境变量、rustup targets、构建约定                   |

## 环境要求

| 工具    | 版本                            |
| ------- | ------------------------------- |
| Node.js | >= 22                           |
| pnpm    | 12.x（`packageManager` 已锁定） |
| Rust    | stable，含 `rustfmt`            |

首次克隆后执行一次 `pnpm install`，会自动注册 git hooks。

## 构建

**按包构建，不跑 `--workspace`**：

```
cargo build -p lxlake-editor
cargo build -p lxlake-demo
```

`cargo build --workspace` 会把 demo 的 `render` 特性统一进 editor，让「框架主线不依赖渲染」这条架构约束失去验证意义。原因详见 [docs/architecture.md](docs/architecture.md)。

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
- 新增实体 crate 时，提交信息正文写明命中[拆包判据](docs/architecture.md)中的哪一条

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

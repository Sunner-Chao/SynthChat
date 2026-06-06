# Git Shell Scripts - Windows PowerShell 版本

用于 Git 和 GitHub 仓库管理的 PowerShell 脚本集合。

## 脚本列表

### create-github-repo.ps1（主要脚本）

**交互式仓库创建与权限管理工具**，一站式完成：
- 创建 GitHub 仓库（支持个人/组织仓库）
- 配置 Git 远程地址
- 初次推送
- 设置仓库可见性（public/private）
- 添加/管理协作者权限

```powershell
# 运行脚本（交互式）
.\create-github-repo.ps1

# 或指定参数
.\create-github-repo.ps1 -Name "my-repo" -Private
```

**交互流程示例：**
```
== 选择仓库可见性 ==
  1. public - 公开 [默认]
  2. private - 私有

请选择 (直接回车选择 1)

== 权限配置 ==
  1. 进入权限配置菜单 [默认]
  2. 跃过配置，完成退出

== 权限配置菜单 ==
  1. 查看当前状态
  2. 设置仓库可见性
  3. 管理协作者权限
  4. 完成配置 [默认]
```

所有步骤都支持**直接回车使用默认值**。

### 其他脚本

| 脚本 | 功能 |
|------|------|
| push-github.ps1 | 推送代码到 GitHub |
| pull-github.ps1 | 从 GitHub 拉取代码 |
| clear-remote-repo.ps1 | 清空远程仓库内容 |
| clear-remote-branch.ps1 | 清空远程分支 |
| clear-tag.ps1 | 清理本地和远程标签 |
| clear-commit.ps1 | 清理提交历史 |
| init-git-env.ps1 | 初始化 Git 环境 |
| set-git-account.ps1 | 设置 Git 账户信息 |
| set-git-remote.ps1 | 设置 Git 远程地址 |
| git-config-helper.ps1 | Git 配置辅助工具 |
| git-quick-status.ps1 | 快速查看 Git 状态 |
| git-script-profile.ps1 | 脚本配置管理 |

## 权限级别说明

| 权限级别 | 说明 |
|---------|------|
| admin | 完全管理权限，包括设置权限、删除仓库 |
| maintain | 维护权限（仅组织仓库），管理 issues、合并 PR |
| write | 写入权限，推送代码、管理 issues 和 PR **[默认]** |
| triage | 分类权限（仅组织仓库），管理 issues 但不能推送 |
| read | 只读权限，查看和拉取代码 |
ssh
## 认证方式ss

### 方式一：GitHub CLI（推荐）

```powershell
# 安装 gh
winget install GitHub.cli

# 登录认证
gh auth login
```

### 方式二：环境变量 Token

```powershell
# 创建 Personal Access Token (需要 repo 权限)
# https://github.com/settings/tokens

# 设置环境变量（当前会话）
$env:GITHUB_TOKEN = 'your_personal_access_token'

# 永久设置（添加到 PowerShell profile）
[Environment]::SetEnvironmentVariable('GITHUB_TOKEN', 'your_token', 'User')
```

## 使用前提

- 需要有仓库的管理权限（admin 或 owner）
- 操作组织仓库需要组织管理员权限
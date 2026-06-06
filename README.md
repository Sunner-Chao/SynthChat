# SynthChat V1.0.0

SynthChat 前端框架 - 独立运行版本

## 项目结构

```
SynthChat-V1.0.0/
├── src/
│   ├── components/     # 通用组件
│   ├── lib/
│   │   ├── api.ts      # API 接口层（当前为 Mock 实现）
│   │   ├── store.ts    # Zustand 状态管理
│   │   └── types.ts    # TypeScript 类型定义
│   ├── panels/         # 页面面板
│   │   ├── ChatExperience.tsx  # 聊天体验
│   │   ├── SettingsPanel.tsx   # 设置面板
│   │   ├── PersonaPanel.tsx    # 角色管理
│   │   ├── MomentsPanel.tsx    # 朋友圈
│   │   └── ToolPanels.tsx      # 工具面板
│   ├── App.tsx         # 主应用组件
│   ├── main.tsx        # 入口文件
│   └── styles.css      # 全局样式
├── package.json        # 项目配置
├── vite.config.ts      # Vite 配置
└── tsconfig.json       # TypeScript 配置
```

## 快速开始

### 安装依赖

```bash
npm install
# 或
pnpm install
```

### 启动开发服务器

```bash
npm run dev
```

访问 http://127.0.0.1:1420 查看应用。

### 构建生产版本

```bash
npm run build
```

## 技术栈

- **React 18** - UI 框架
- **Zustand** - 状态管理
- **Vite** - 构建工具
- **TypeScript** - 类型安全
- **Lucide React** - 图标库

## API 层

当前 `src/lib/api.ts` 使用 Mock 实现，所有 API 调用都会返回模拟数据。

### 连接真实后端

要连接真实的后端服务，需要：

1. 修改 `src/lib/api.ts` 中的 `mockInvoke` 函数
2. 将 Mock 实现替换为真实的 HTTP 请求
3. 或者使用 Tauri 的 `invoke` 函数连接桌面后端

### API 接口列表

- `getConfig()` - 获取应用配置
- `saveConfig(config)` - 保存应用配置
- `listPersonas()` - 获取角色列表
- `listConversations()` - 获取会话列表
- `sendChatMessage(request)` - 发送聊天消息
- `listLlmProviders()` - 获取 LLM 提供商列表
- `listMcpServers()` - 获取 MCP 服务器列表
- ... 更多接口见 `api.ts`

## 后续计划

1. **后端框架搭建** - 基于 LangGraph 的 Agent 编排后端
2. **API 对接** - 连接前端与后端
3. **功能完善** - 补充所有功能实现

## 相关链接

- [SynthChat V0.1.8](../SynthChat-V0.1.8) - 原始项目（含 Tauri 后端）
- [LangGraph Rust](https://github.com/Onelevenvy/langgraph-rust) - LangGraph Rust 实现

# Moe Sekai API

Moe Sekai API 是一个基于 Rust 的自部署 API 服务，面向需要统一接入多区服游戏接口的开发与维护场景。

本项目基于 [Team-Haruki/Haruki-Sekai-API](https://github.com/Team-Haruki/Haruki-Sekai-API) 进行修改与整理，并在命名、配置方式、容器化与文档组织上做了适配性调整。

## 说明

本仓库仅提供源码、基础运行方式与公开示例配置。

以下内容**不会**随仓库提供：
- 任何真实账号文件
- 任何生产可用密钥、Cookie、设备标识或请求头样本
- 任何私有运行数据、数据库文件或版本文件
- 任何现网部署信息、业务状态或内部运维细节

如果你要运行本项目，需要自行准备你自己的配置、账号与运行环境。

## 快速开始

### 本地运行

1. 准备 Rust 工具链。
2. 构建项目：

```bash
cargo build --release
```

3. 将 `moe-sekai-configs.example.yaml` 复制为你的私有运行配置，例如：

```bash
cp moe-sekai-configs.example.yaml moe-sekai-configs.yaml
```

4. 按你的环境填写配置。
5. 启动服务：

```bash
./target/release/moe-sekai-api
```

如需指定配置文件路径：

```bash
CONFIG_PATH=/path/to/moe-sekai-configs.yaml ./target/release/moe-sekai-api
```

如需临时覆盖监听端口：

```bash
PORT=8080 ./target/release/moe-sekai-api
```

### Docker

仓库根目录包含 `Dockerfile`，可直接构建：

```bash
docker build -t moe-sekai-api .
```

一个最小运行示例：

```bash
docker run -d \
  -p 9999:9999 \
  -v $(pwd)/data:/data \
  --name moe-sekai-api \
  moe-sekai-api
```

默认约定：
- 容器内配置路径：`/data/moe-sekai-configs.yaml`
- 默认端口：`9999`
- 健康检查接口：`/health`

## 配置

默认配置文件名：

- `moe-sekai-configs.yaml`

公开仓库中的配置入口：

- `moe-sekai-configs.example.yaml`

请注意：
- `moe-sekai-configs.example.yaml` 仅为公开示例，不包含真实可用配置
- 真实运行配置不应提交到仓库
- 所有密钥、凭据、设备标识、Cookie 与账号文件都应自行提供

常用环境变量：
- `CONFIG_PATH`：指定配置文件路径
- `PORT`：覆盖监听端口

访问受保护路由时使用请求头：
- `x-moe-sekai-token`

### 代理账号标记

账号 JSON 支持可选字段 `proxyRoles`，用于标记该游戏账号可以代理哪些类型的接口：

```json
{
  "userId": "123456789",
  "deviceId": "...",
  "credential": "...",
  "proxyRoles": ["mysekai"]
}
```

未配置 `proxyRoles` 的账号默认只用于普通接口。`/mysekai/` 相关路由会使用 `proxyRoles` 包含 `mysekai` 的账号代理，例如 `POST /api/{server}/user/mysekai/housing-competition/{housing_id}/mysekai-owner/{owner_user_id}/entry`。

上游 HTTP 代理可以在顶层 `proxy` 中设置为所有服务器的默认值，也可以在 `servers.<region>.proxy` 中按服务器单独设置。服务器级配置优先于顶层配置；省略服务器级字段表示继承顶层代理，显式配置为空字符串表示该服务器直连。例如：

```yaml
proxy: ""
servers:
  tw:
    proxy: "http://moe-upstream-proxy:7890"
  kr:
    proxy: "http://moe-upstream-proxy:7890"
```

## 目录建议

建议将运行时数据与源码分离。一个常见布局如下：

```text
runtime/
├── moe-sekai-configs.yaml
├── accounts/
├── master/
└── versions/
```

或者在容器中统一使用：

```text
/data/
├── moe-sekai-configs.yaml
├── accounts/
├── master/
└── versions/
```

## 安全与开源边界

为避免误泄露，以下文件或目录不应提交：
- 真实配置文件
- 账号目录
- 本地数据库文件
- 本地日志文件
- 运行时主数据与版本数据

本仓库中的示例配置与文档会尽量保持通用与保守，不会包含真实业务环境信息。

## 上游说明

本项目基于 [Team-Haruki/Haruki-Sekai-API](https://github.com/Team-Haruki/Haruki-Sekai-API) 进行修改与整理，并在配置方式、容器化与文档组织上做了进一步调整。

## License

上游项目采用 **MIT License** 发布，本项目在保留原许可要求的前提下继续进行修改与整理。

This project is licensed under the MIT License.

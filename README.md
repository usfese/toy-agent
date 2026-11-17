# toy-agent

一个使用 OpenAI Chat Completions 接口的命令行玩具 agent。默认启动交互式调试前端。目前没有任何生产价值。

客户端配置模板为 `config.example.json`。Debug 版本可复制模板并填入自己的配置：

```sh
mkdir -p .toy-agent
cp config.example.json .toy-agent/config.json
chmod 600 .toy-agent/config.json
```

API key 只从配置文件读取，`OPENAI_API_KEY` 不再生效。`OPENAI_MODEL` 和 `OPENAI_BASE_URL` 仍可覆盖文件中的对应字段。Debug 默认使用当前工作目录下的 `.toy-agent/config.json`，也可通过 `TOY_AGENT_CONFIG` 指定配置文件。

```sh
export OPENAI_MODEL=your-model
# 交互前端
cargo run
# 单次调用、列出会话
cargo run -- chat my-session '你好'
cargo run -- list
```

会话操作会对同一会话加锁；会话文件包含对话与工具输出，不应共享。Bash 工具调用需要逐次审批，最长执行 120 秒，stdout 和 stderr 合计最多保留 1 MiB；超时会终止该进程组，超限输出会标记截断。这不是沙箱。CLI 里直接输入的 `!` 命令不受这些工具限制。

Unix 下，新建会话目录和文件分别使用 `0700` 和 `0600`。现有目录或会话文件权限过宽时程序会拒绝访问；请确认目录和文件属于自己且没有硬链接后，将目录权限调整为 `0700`、会话文件调整为 `0600`。同一会话正在被其他进程使用时，新的对话请求会报锁冲突，不会覆盖正在进行的会话。

模型返回截断或非成功的结束状态时会报错，不会把不完整回答保存为最终回复。达到模型调用次数上限时，已完成的工具结果保留；恢复对话不会自动重放执行结果未知的工具调用。

# toy-agent

一个使用 OpenAI Chat Completions 接口的命令行玩具 agent。默认启动交互式调试前端。目前没有任何生产价值。

客户端配置模板为 `config.example.json`。Debug 版本可复制模板并填入自己的配置：

```sh
mkdir -p .toy-agent
cp config.example.json .toy-agent/config.json
chmod 600 .toy-agent/config.json
```

```sh
# 或使用环境变量，不需要配置文件
export OPENAI_API_KEY=your-key
export OPENAI_MODEL=your-model
# 交互前端
cargo run
# 单次调用、列出会话
cargo run -- chat my-session '你好'
cargo run -- list
```

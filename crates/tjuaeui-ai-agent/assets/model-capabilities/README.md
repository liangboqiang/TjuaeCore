# 图像输入模型白名单

`image_input_models.json` 是人工维护并随 TjuaeCore 编译嵌入的静态资产，运行时不会
下载或刷新。API 根地址与 TjuaeUI `modelPlatforms.ts` 中的固定 `base_url` 预设保持
一致；模型 ID 独立维护，不从第三方模型目录自动导入。

本目录采用严格的正向白名单：

- 必须同时匹配供应商 API 根地址和精确模型 ID。
- 只有供应商官方文档确认当前 API 协议支持图像输入时，才能加入模型。
- 供应商或模型不在列表中表示 `Unknown`，不能据此断言不支持图像。
- 不得把第一方模型条目直接复制到聚合平台或自定义网关；这些端点可能暴露不同的
  模型 ID 或能力。
- 已知预设端点但没有可稳定验证的模型 ID 时，保留空 `models` 数组。这只记录
  TjuaeUI 预设，不宣称支持图像。
- 聚合平台条目只能根据该平台自己的目录人工更新；复核结果必须提交为静态快照，
  TjuaeCore 不在运行时拉取。

Poe 机器人名称和天翼云部署模型 ID 由账户或部署决定，因此不提供静态模型条目。
DeepSeek 对应的 TjuaeUI 预设端点目前没有经官方资料明确验证的图像输入聊天模型。

白名单最近一次复核日期为 2026-07-15，依据以下供应商资料：

- OpenAI：https://developers.openai.com/api/docs/models
- Anthropic 与 Bedrock 模型 ID：https://platform.claude.com/docs/en/about-claude/models/overview
- Amazon Bedrock 图像消息：https://docs.aws.amazon.com/bedrock/latest/userguide/model-parameters-anthropic-claude-messages.html
- Gemini：https://ai.google.dev/gemini-api/docs/models
- TjuaeUI 供应商预设：https://github.com/liangboqiang/TjuaeUI/blob/main/packages/desktop/src/renderer/utils/model/modelPlatforms.ts
- Novita：https://novita.ai/models 与 https://novita.ai/docs/guides/llm-vision
- OpenRouter：https://openrouter.ai/api/v1/models 与 https://openrouter.ai/docs/guides/overview/multimodal/image-understanding
- MiniMax：https://platform.minimaxi.com/docs/api-reference/text/api/openapi-chat-openai.json
- 阿里云百炼：https://help.aliyun.com/en/model-studio/vision-model/
- SiliconFlow：https://www.siliconflow.com/models/vision 与 https://docs.siliconflow.cn/cn/userguide/capabilities/multimodal-vision
- 智谱：https://docs.bigmodel.cn/cn/guide/models/vlm/glm-5v-turbo
- Moonshot：https://platform.kimi.ai/docs/models
- xAI：https://docs.x.ai/developers/model-capabilities/images/understanding
- 火山方舟：https://www.volcengine.com/docs/82379/1795150
- 百度千帆：https://cloud.baidu.com/doc/qianfan-docs/s/fm8r1ndsm
- 腾讯混元：https://cloud.tencent.com/document/product/1729/104753 与 https://cloud.tencent.com/document/product/1729/111007
- 零一万物：https://platform.lingyiwanwu.com/
- PPIO：https://ppio.com/docs/model/visual 与 https://ppio.com/pricing
- ModelScope：https://www.modelscope.cn/docs/model-service/API-Inference/intro
- InfiniAI：https://docs.infini-ai.com/gen-studio/api/multimodal/tutorial-vision.html
- 天翼云：https://www.ctyun.cn/document/10541165/10876778
- 阶跃星辰：https://platform.stepfun.com/docs/zh/guides/models/vision

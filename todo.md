# Memory Migration TODO

目标：把当前自研 `L1/L2/L3` 记忆架构迁移为：
- `L1`：更原始的 working messages 流
- 长期记忆：Hindsight `retain / recall / reflect`
- `sleep`：保留为复盘思考程序，不再承担本地长期记忆后端

当前进度：
- 已完成：Hindsight client/config、runtime pre-step recall/reflect、raw L1 step journal、snapshot 去记忆化、本地 L2/L3 主路径拆除、sleep reflection retain
- 已完成：LLM 接口显式拆成 `system_messages / long_term_memory_messages / history_messages / current_user_message`
- 已完成：retain 改成异步 worker 队列，shutdown 时 flush
- 待继续：按新架构补训练/验证用例，并继续收紧 L1/retain 边界

## 0. 设计冻结
- 停止继续扩展本地 `L2/L3` 功能
- 不再新增基于 `memory.search_mem/search_l3` 的新调用点
- 不再继续强化 `sleep_artifacts -> l3_memory` 这条旧链路

## 1. 重做 L1 为原始消息流
- 替换 [src/memory.rs](C:/Users/13940/spinova/src/memory.rs) 当前 `L1Memory`
  - 去掉“每次由 LLM 重写 thread_focus/event_summary”的强语义压缩模型
  - 改为 ring buffer 形式的近场消息流
- 新 `L1` 应至少保留：
  - 最近动作
  - 最近观察
  - 当前 phase
  - 当前设备/前景状态
  - 最近一次终端命令与结果
- 明确 `L1` 不做 recall，不写入长期记忆检索接口

## 2. 终端进入 working context，但要双层表示
- 终端相关内容进入运行时 working context，但拆成两层：
  - 原始 recent terminal messages
  - 压缩后的 terminal summary
- 需要显式表示：
  - prompt 是否可见
  - 当前是否还在 pager / running process / shell
  - 最近命令是否已完成
- 不再让“终端状态判断”依赖隐式总结或自动联想

## 3. 删除本地 L2/L3 存储与显式召回
- 删除 [src/memory.rs](C:/Users/13940/spinova/src/memory.rs) 中：
  - `L2Memory`
  - `L3Memory`
  - `search_mem`
  - `search_l3`
  - `upsert_l3_entries`
  - `sync_l3_to_disk`
- 删除 runtime 中基于本地记忆的显式召回
- 清理 [src/tasks.rs](C:/Users/13940/spinova/src/tasks.rs) 中工作任务的 recall 缓存字段

## 4. 引入 Hindsight backend
- 新增 Rust 侧 Hindsight client/back-end 抽象
- 需要最小接口：
  - `retain(messages, metadata)`
  - `recall(query, top_k, tags)`
  - `reflect(query, scope, top_k)`
- 配置放入 [src/config.rs](C:/Users/13940/spinova/src/config.rs)
  - `HINDSIGHT_BASE_URL`
  - `HINDSIGHT_API_KEY`
  - bank/session 配置
- 统一错误处理与超时，避免阻塞整个 runtime loop

## 5. 按官方 wrapper 语义接入上下文
- 参考 Hindsight 官方 wrapper 的真实行为：
  - pre-call: `recall/reflect`
  - post-call: `retain`
  - retain 默认异步
- 但不要机械照搬到每个微小子程序
- 首版只在粗粒度决策点注入：
  - 每个 runtime step 前一次 recall/reflect
  - phase 切换时可额外 recall
- 注入位置对齐 runtime prompt 构建层，显式传入：
  - `history_messages` (`L1`)
  - `long_term_memory_messages`
  - `current_user_message` (`snapshot`)

## 6. 重写 runtime retain 时机
- 不要每个微 step 都同步 retain
- 当前方向：
  - `L1` 接近淘汰边界时自动 enqueue raw retain
  - 真到要 drop 但还未 retain 完时再背压 flush
  - `sleep` 复盘经验单独桥接到 Hindsight
- 不再保留 phase / step conclusion / episode summary 这三类 runtime 特殊 retain
- retain 通过异步 worker 队列写入 Hindsight，shutdown 时 flush

## 7. 重构 sleep：一边产优化输入，一边产复盘文本
- 重写 [src/reasoning/sleep.rs](C:/Users/13940/spinova/src/reasoning/sleep.rs)
- 保留 `sleep` 的双重作用，但拆清两条输出：
  - 输出 A：`sleep_artifacts`
    - 继续作为 `optimize reasoning` 的直接输入
    - 包括 demo / stress / instruction / 其它可编译资产
  - 输出 B：高质量复盘文本
    - 交给 Hindsight retain
    - 充当之前 `L3` 那类长期反思经验
- 删除本地：
  - `failure_patterns -> l3 promotion`
  - `l3_memory` 写盘
  - 本地 `L2/L3` 作为长期记忆后端
- 明确不要做的事：
  - 不让 `optimize` 直接消费 Hindsight `reflect`
  - 不用 Hindsight `reflect` 替代 `sleep_artifacts`

## 8. 明确保留与删除的边界
- 保留：
  - `sleep` 作为主动复盘思考
  - `sleep_artifacts -> optimize reasoning` 作为编译/评估层
- 删除：
  - 本地 `L2/L3` 记忆库
  - 本地 recall/promoter/upsert 流程
- 明确新增：
  - Hindsight 负责长期保存/召回复盘经验
  - 这些复盘经验等价于过去 `L3` 想承担的那层语义

## 9. 调整 snapshot 与 prompt 结构
- `snapshot` 只承载当前状态：
  - 当前设备/终端状态
  - 当前任务脚手架
  - 当前义务/项目/next actions
- `L1` 作为真实 `history_messages`
- Hindsight 作为 `long_term_memory_messages`
- 删除当前“联想回忆 / 习得经验”文案，避免继续暗示本地 `L2/L3`

## 10. 清理旧训练/评测路径
- 检查并替换：
  - [src/reasoning/bench/datasets/memory_recall.rs](C:/Users/13940/spinova/src/reasoning/bench/datasets/memory_recall.rs)
  - [src/reasoning/bench/programs/memory_recall.rs](C:/Users/13940/spinova/src/reasoning/bench/programs/memory_recall.rs)
  - [src/reasoning/bench/optimize.rs](C:/Users/13940/spinova/src/reasoning/bench/optimize.rs)
- 这些 benchmark 若仍保留，应改成：
  - Hindsight recall/reflect benchmark
  - 或直接删除旧本地 recall benchmark

## 11. 新的迁移顺序
- 第一步：落 Hindsight client 与配置
- 第二步：重做 `L1`
- 第三步：在 runtime step 前接入 Hindsight recall
- 第四步：在 episode/phase 后接入异步 retain
- 第五步：把 `sleep` 改成复盘 retain
- 第六步：删本地 `L2/L3`
- 第七步：清理旧 benchmark / optimize 依赖

## 12. 完成标准
- runtime 不再依赖 `memory.search_mem/search_l3`
- `Snapshot` 不再展示任何本地联想回忆或历史记忆
- `sleep` 不再写本地 `l3_memory`
- runtime 能从 Hindsight 注入长期上下文
- training / normal runtime 使用同一套 Hindsight 长期记忆后端

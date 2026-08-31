# 自动化测试的用户存储隔离

## 范围与责任

离线自动化测试不得读取或写入开发者的 Sigil 配置、凭证、启动元数据、会话、资源账本、模型记录或缓存。测试隔离属于 non-shipping 工程入口，不属于 kernel/runtime 的产品权限机制：不增加生产 test mode，不放宽 approval，不改变路径解析优先级，也不改变普通 `cargo run` 的行为。

该约束落实[工程规范](../governance/engineering-standards.md)的验证要求，并保持[核心架构](sigil-rust-agent-core-technical-solution.md)的配置、凭证与受管存储责任边界。

## 两层隔离

1. **执行入口负责用户身份隔离。** `python3 scripts/run-isolated-tests.py -- <command...>` 为本次子进程提供全新的临时用户目录，隔离 HOME、USERPROFILE 及平台用户存储环境；清除继承的 Sigil storage/scratch override。子进程继续继承隔离后的身份，不复制用户配置或凭证。Cargo/Rustup 工具链目录单独保留，不能因此恢复真实 Sigil 用户根。
2. **测试夹具负责资源与并发隔离。** 每个会发生存储副作用的 fixture 显式拥有自己的临时 storage/config/workspace。启动 child 时通过该 child 的环境传入，不在并发测试进程中临时修改 HOME。确需测试环境变量优先级的用例必须使用同域环境锁，并完整恢复变量；锁不等于整个进程的所有环境读取已经串行化。

不能统一设置一个 `SIGIL_STATE_HOME` / `SIGIL_CACHE_HOME` 来代替这两层：这些变量优先于 fixture 的显式 storage 配置，会把本来独立的测试重新导向同一个目录。只隔离 storage 而不隔离用户目录，也会继续触达真实 bootstrap 和凭证存储。

## 运行与验收

```bash
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-runtime
python3 scripts/run-isolated-tests.py -- cargo test --doc
python3 scripts/run-isolated-tests.py -- path/to/already-built-test-binary <test-filter>
```

仓库 gate、coverage 和离线 conformance 应调用同一隔离入口；独立脚本启动产品 child 时也必须进入该入口或使用等价且经测试的独立 fixture 环境。仅有静态扫描通过不代表测试没有副作用。

离线验收脚本的显式 `--keep-*` 仍须保留调试夹具：这些夹具放在仓库 `.repo-local-dev/test-artifacts/` 下的独立目录，脚本输出实际位置；普通调用的夹具继续随临时根清理。保留某个夹具不等于保留整份临时用户目录，也不能让该路径获得嵌套入口的身份信任。

验收必须包含真实 child/grandchild、并发隔离、继承 override 清除、原调用方哨兵不变、失败退出码透传，以及相关产品 fixture 的非零测试结果。用例不得通过删除断言、全套改为单线程、跳过失败或伪造路径解析结果获得通过。

这是环境隔离，不是对恶意代码的 OS 沙箱；绕过入口的裸 `cargo test` 或手动 binary 不自动受保护。显式真实 provider / OS keyring / 发布资格化需要专用授权环境，不能用离线隔离结果冒充这些验证。

嵌套入口只能复用 marker 完整且用户路径实际位于该临时根内的环境；不能因为路径位于系统临时目录就信任它。POSIX 中断转发限于本次子进程的进程组，不承诺回收自行脱离进程组的后代；Windows 进程树停止也不由本入口证明。

发布资格化保留自己的临时 HOME、平台前置条件及证据输出流程。它调用的普通离线 gate 可以各自创建并清理隔离根；资格化的 bootstrap residue 计数仅描述其自身 HOME，不是所有子 gate 的全局残留清单。

## 历史目录

2026-08-31 的只读核查确认，上一批未完整隔离的测试已在真实默认目录留下新增数据，并触达共享索引、资源账本、最近模型记录及权限元数据。没有测试前快照，不能声明历史数据完全未变。

本次整改只阻止后续测试再次使用真实默认目录。既有目录保持现状，不根据时间戳猜测删除范围，不迁移、重签或回滚未知的旧权限；实际存储切换仍遵守已批准的 fresh managed storage 决策，不能由测试修复暗中执行。

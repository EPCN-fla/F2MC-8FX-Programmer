//! 构建脚本：Windows 下把 exe 图标/版本信息嵌入 PE 资源（embed-resource）
//! 非 Windows 目标自动为空操作。

fn main() {
    // 仅 Windows 目标生效；embed-resource 内部对非 Windows 自动 no-op
    embed_resource::compile("assets/f2mc.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

//! 构建脚本：Windows 下嵌入 exe 图标与版本信息资源（embed-resource）
//! 非 Windows 目标自动为空操作。

fn main() {
    embed_resource::compile("assets/f2mc-cli.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

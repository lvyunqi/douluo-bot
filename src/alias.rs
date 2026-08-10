/// 可由玩家快捷键展开的普通游戏主命令；不包含宿主或游戏管理命令。
pub const PLAYER_ALIAS_TARGET_COMMANDS: &[&str] = &[
    "斗罗系统",
    "开始穿越",
    "武魂觉醒",
    "开武魂",
    "关武魂",
    "技能",
    "技能详情",
    "装备魂技",
    "卸下魂技",
    "魂环",
    "吸收魂环",
    "剥离魂环",
    "释放技能",
    "签到",
    "钱包",
    "转账",
    "NPC",
    "对话",
    "商店",
    "背包",
    "储物器",
    "查看储物器",
    "存入",
    "取出",
    "封印储物器",
    "解封储物器",
    "装备魂导器",
    "卸下魂导器",
    "购买",
    "出售",
    "使用",
    "发送物品",
    "状态",
    "位置",
    "地图列表",
    "数值曲线",
    "向",
    "传送",
    "掉落",
    "拾取",
    "任务",
    "接取任务",
    "任务进度",
    "提交任务",
    "放弃任务",
    "魂兽",
    "挑战",
    "攻击",
    "逃跑",
    "战斗状态",
    "战斗日志",
    "决斗",
    "决斗状态",
    "接受决斗",
    "取消决斗",
];

const REGISTERED_GAME_COMMAND_KEYS: &[&str] = &[
    "斗罗系统",
    "斗罗菜单",
    "菜单",
    "开始穿越",
    "开始转生",
    "武魂觉醒",
    "觉醒",
    "开武魂",
    "武魂开启",
    "关武魂",
    "武魂关闭",
    "技能",
    "魂技",
    "技能列表",
    "技能详情",
    "装备魂技",
    "装备技能",
    "卸下魂技",
    "卸下技能",
    "魂环",
    "查看魂环",
    "吸收魂环",
    "附加魂环",
    "剥离魂环",
    "剥离",
    "释放技能",
    "使用技能",
    "使用魂技",
    "施放魂技",
    "签到",
    "每日签到",
    "打卡",
    "钱包",
    "我的钱包",
    "余额",
    "转账",
    "转钱",
    "汇款",
    "NPC",
    "人物",
    "当前NPC",
    "对话",
    "交谈",
    "聊天",
    "商店",
    "店铺",
    "背包",
    "随身物品",
    "物品",
    "道具",
    "储物器",
    "储物",
    "储物器列表",
    "查看储物器",
    "打开储物器",
    "存入",
    "取出",
    "封印储物器",
    "解封储物器",
    "解封",
    "装备魂导器",
    "装备储物器",
    "卸下魂导器",
    "卸下储物器",
    "购买",
    "购买物品",
    "买",
    "出售",
    "卖出",
    "卖",
    "使用",
    "使用物品",
    "用",
    "发送物品",
    "赠送",
    "赠送物品",
    "状态",
    "我的状态",
    "属性",
    "位置",
    "地图",
    "当前位置",
    "地图列表",
    "地图清单",
    "数值曲线",
    "成长曲线",
    "曲线列表",
    "向",
    "传送",
    "掉落",
    "查看掉落",
    "地面掉落",
    "拾取",
    "捡取",
    "拾取物品",
    "任务",
    "任务列表",
    "任务清单",
    "接取任务",
    "接受任务",
    "接任务",
    "任务进度",
    "我的任务",
    "进行中任务",
    "提交任务",
    "完成任务",
    "交任务",
    "放弃任务",
    "取消任务",
    "魂兽",
    "魂兽列表",
    "当前魂兽",
    "挑战",
    "挑战魂兽",
    "攻击",
    "打",
    "逃跑",
    "撤退",
    "战斗状态",
    "战斗",
    "查看战斗",
    "战斗日志",
    "决斗",
    "决斗状态",
    "接受决斗",
    "取消决斗",
    "旧档检查",
    "旧档认领",
    "授权上下文",
    "新增授权",
    "授权群",
    "取消授权",
    "撤销授权",
    "删除授权",
    "查看授权",
    "授权列表",
    "设置快捷键",
    "快捷键列表",
    "查看快捷键",
    "删除快捷键",
];

const HOST_RESERVED_COMMAND_KEYS: &[&str] = &[
    "plugins",
    "pl",
    "registry",
    "reg",
    "dynamic-errors",
    "derr",
    "help",
    "h",
    "?",
];

/// 判断目标是否为可安全展开的游戏主命令。
pub fn is_player_alias_target(command: &str) -> bool {
    PLAYER_ALIAS_TARGET_COMMANDS.contains(&command)
}

/// 校验设置目标，拒绝宿主管理、游戏管理和二次别名展开。
pub fn validate_player_alias_target(command: &str) -> Result<(), String> {
    if is_player_alias_target(command) {
        Ok(())
    } else {
        Err("原指令必须是可执行的游戏主命令，不能指向管理命令、宿主命令或其他快捷键".to_string())
    }
}

/// 校验快捷键名称不遮蔽当前已注册命令，保证普通命令始终优先且名称可实际触发。
pub fn validate_player_alias_name(alias: &str) -> Result<(), String> {
    if REGISTERED_GAME_COMMAND_KEYS.contains(&alias) {
        return Err("快捷键不能与已注册的斗罗命令或别名重名".to_string());
    }
    if HOST_RESERVED_COMMAND_KEYS.contains(&alias) {
        return Err("快捷键不能覆盖 QimenBot 管理或帮助命令".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_normal_game_commands_can_be_alias_targets() {
        assert!(is_player_alias_target("状态"));
        assert!(is_player_alias_target("释放技能"));
        assert!(is_player_alias_target("数值曲线"));
        assert!(!is_player_alias_target("旧档认领"));
        assert!(is_player_alias_target("\u{50a8}\u{7269}\u{5668}"));
        assert!(is_player_alias_target(
            "\u{67e5}\u{770b}\u{50a8}\u{7269}\u{5668}"
        ));
        assert!(!is_player_alias_target("plugins"));
    }

    #[test]
    fn reserved_command_keys_cannot_be_reused_as_player_aliases() {
        assert!(validate_player_alias_name("状态").is_err());
        assert!(validate_player_alias_name("设置快捷键").is_err());
        assert!(validate_player_alias_name("\u{50a8}\u{7269}\u{5668}").is_err());
        assert!(validate_player_alias_name("\u{6253}\u{5f00}\u{50a8}\u{7269}\u{5668}").is_err());
        assert!(validate_player_alias_name("plugins").is_err());
        assert!(validate_player_alias_name("查询状态").is_ok());
    }
}

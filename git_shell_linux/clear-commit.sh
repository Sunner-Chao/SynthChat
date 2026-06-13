#!/bin/bash

# 一键清空 Git 所有 commit 提交历史，保留现有代码，重置为初始提交

set -e

echo -e "\033[36m========================================\033[0m"
echo -e "\033[31m  即将清空Git全部Commit历史（危险操作）\033[0m"
echo -e "\033[36m========================================\033[0m"
read -p "输入 YES 确认执行，其他直接退出: " confirm

if [ "$confirm" != "YES" ]; then
    echo -e "\033[32m已取消操作\033[0m"
    exit 0
fi

# 1. 创建空孤儿分支（无历史）
if ! git checkout --orphan temp_clear_history; then
    echo "创建临时孤儿分支失败。" >&2
    exit 1
fi

# 2. 暂存所有代码
if ! git add .; then
    echo "git add 失败。" >&2
    exit 1
fi

# 3. 生成全新初始提交
if ! git commit -m "init: 重置仓库，清空所有历史提交"; then
    echo "git commit 失败。" >&2
    exit 1
fi

# 4. 删除旧主分支
if ! git branch -D main; then
    echo "删除旧 main 分支失败。" >&2
    exit 1
fi

# 5. 重命名临时分支为主分支
if ! git branch -m main; then
    echo "重命名临时分支为 main 失败。" >&2
    exit 1
fi

echo ""
echo -e "\033[33m本地历史已清空，准备强制推送到远程！\033[0m"
read -p "再次输入 YES 强制覆盖远程仓库历史: " push_confirm

if [ "$push_confirm" = "YES" ]; then
    if ! git push -f origin main; then
        echo "强制推送远程失败。" >&2
        exit 1
    fi
    echo ""
    echo -e "\033[32m远程仓库历史已彻底重置完成！\033[0m"
else
    echo -e "\033[31m已取消推送，仅本地生效\033[0m"
fi

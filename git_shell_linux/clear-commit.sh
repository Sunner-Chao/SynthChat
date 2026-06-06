#!/bin/bash

# 一键清空 Git 所有 commit 提交历史，保留现有代码，重置为初始提交

echo -e "\033[36m========================================\033[0m"
echo -e "\033[31m  即将清空Git全部Commit历史（危险操作）\033[0m"
echo -e "\033[36m========================================\033[0m"
read -p "输入 YES 确认执行，其他直接退出: " confirm

if [ "$confirm" != "YES" ]; then
    echo -e "\033[32m已取消操作\033[0m"
    exit 0
fi

# 1. 创建空孤儿分支（无历史）
git checkout --orphan temp_clear_history

# 2. 暂存所有代码
git add .

# 3. 生成全新初始提交
git commit -m "init: 重置仓库，清空所有历史提交"

# 4. 删除旧主分支
git branch -D main

# 5. 重命名临时分支为主分支
git branch -m main

echo ""
echo -e "\033[33m本地历史已清空，准备强制推送到远程！\033[0m"
read -p "再次输入 YES 强制覆盖远程仓库历史: " push_confirm

if [ "$push_confirm" = "YES" ]; then
    git push -f origin main
    echo ""
    echo -e "\033[32m远程仓库历史已彻底重置完成！\033[0m"
else
    echo -e "\033[31m已取消推送，仅本地生效\033[0m"
fi
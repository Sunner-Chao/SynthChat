<#
.SYNOPSIS
一键清空Git所有commit提交历史，保留现有代码，重置为初始提交
#>

# 禁止错误中断
$ErrorActionPreference = "SilentlyContinue"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  即将清空Git全部Commit历史（危险操作）" -ForegroundColor Red
Write-Host "========================================" -ForegroundColor Cyan
$confirm = Read-Host "输入 YES 确认执行，其他直接退出"

if ($confirm -ne "YES") {
    Write-Host "已取消操作" -ForegroundColor Green
    exit 0
}

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

Write-Host "`n本地历史已清空，准备强制推送到远程！" -ForegroundColor Yellow
$pushConfirm = Read-Host "再次输入 YES 强制覆盖远程仓库历史"

if ($pushConfirm -eq "YES") {
    git push -f origin main
    Write-Host "`n✅ 远程仓库历史已彻底重置完成！" -ForegroundColor Green
}
else {
    Write-Host "❌ 已取消推送，仅本地生效" -ForegroundColor Red
}

# 恢复错误提示
$ErrorActionPreference = "Continue"
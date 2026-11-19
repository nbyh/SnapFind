using SnapFind.Desktop.Ipc;
using SnapFind.Desktop.Services;
using SnapFind.Desktop.ViewModels;
using Xunit;

namespace SnapFind.Desktop.Tests;

/// <summary>
/// ConfigIsolatedTest 用进程级环境变量 SNAPFIND_CONFIG 隔离配置，
/// 并行执行会互相覆盖；所有继承它的测试统一串行。
/// </summary>
[CollectionDefinition("ConfigIsolated", DisableParallelization = true)]
public sealed class ConfigIsolatedCollection;

/// <summary>
/// 回归：睡眠恢复后后端 scope.preview 曾返回重复卷 ID，UI 把重复行写进
/// Rows.ToDictionary 时抛 "An item with the same key has already been added"，
/// 被外层误报成「索引服务不可用」，且点「重试」因不重启进程而无限复现。
/// </summary>
[Collection("ConfigIsolated")]
public class IndexViewModelTests : ConfigIsolatedTest
{
    private static ScopePreview Scope(params (string id, string mount)[] vols) => new()
    {
        Volumes = vols
            .Select(v => new ScopeVolume { VolumeId = v.id, Mount = v.mount })
            .ToList(),
    };

    private static ShellViewModel NewShell() => new(AppConfig.Load(), null);

    [Fact]
    public void ApplyScope_DeduplicatesRepeatedVolumeIds()
    {
        var index = NewShell().Index;

        // 后端返回同一个卷 ID 两次（睡眠恢复后不同挂载点枚举）
        index.ApplyScope(Scope(("vol-a", "/"), ("vol-a", "/mnt/a")));
        Assert.Single(index.Rows);
        Assert.Equal("/", index.Rows[0].Mount); // 只保留第一条

        // 下一次刷新（点重试/重新扫描磁盘）不再抛 same-key 异常
        index.ApplyScope(Scope(("vol-a", "/")));
        Assert.Single(index.Rows);
    }

    [Fact]
    public void ApplyScope_WithPreexistingDuplicateRows_DoesNotThrow()
    {
        var index = NewShell().Index;
        // 模拟旧版本遗留的脏状态：Rows 里已有重复键
        index.Rows.Add(new VolumeRow { VolumeId = "vol-a", Mount = "/", Selected = true });
        index.Rows.Add(new VolumeRow { VolumeId = "vol-a", Mount = "/mnt/a" });

        index.ApplyScope(Scope(("vol-a", "/")));

        Assert.Single(index.Rows);
        Assert.True(index.Rows[0].Selected);
    }

    [Fact]
    public void ResetForReconnect_ClearsDerivedState()
    {
        var index = NewShell().Index;
        index.ApplyScope(Scope(("vol-a", "/")));
        index.Rows.Add(new VolumeRow { VolumeId = "vol-a", Mount = "/dup" });
        index.ScopeError = "索引服务不可用：...";
        index.HasScopeError = true;

        index.ResetForReconnect();

        Assert.Empty(index.Rows);
        Assert.Empty(index.ProgressRows);
        Assert.False(index.HasScopeError);
        Assert.Equal("", index.ScopeError);
        // 清空后再从新进程重建也不会因残留重复键抛异常
        index.ApplyScope(Scope(("vol-a", "/")));
        Assert.Single(index.Rows);
    }
}

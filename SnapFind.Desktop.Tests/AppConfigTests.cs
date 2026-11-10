using SnapFind.Desktop.Helpers;
using SnapFind.Desktop.Ipc;
using SnapFind.Desktop.Services;
using SnapFind.Desktop.ViewModels;
using Xunit;

namespace SnapFind.Desktop.Tests;

/// <summary>每个测试用独立的 config.json（SNAPFIND_CONFIG 指向临时目录），互不污染。</summary>
public abstract class ConfigIsolatedTest : IDisposable
{
    protected string ConfigPath { get; }

    protected ConfigIsolatedTest()
    {
        ConfigPath = Path.Combine(Path.GetTempPath(), $"snapfind-test-{Guid.NewGuid():N}.json");
        Environment.SetEnvironmentVariable("SNAPFIND_CONFIG", ConfigPath);
    }

    public void Dispose() => Environment.SetEnvironmentVariable("SNAPFIND_CONFIG", null);
}

public class AppConfigTests : ConfigIsolatedTest
{
    [Fact]
    public void Save_Then_Load_Preserves_All_Values()
    {
        var cfg = AppConfig.Load();
        cfg.Theme = "dark";
        cfg.CloseToTray = false;
        cfg.ParallelVolumes = 4;
        cfg.DailyScan = true;
        cfg.DailyScanAt = "04:30";
        cfg.IndexRoot = @"D:\SnapFindIndex";
        cfg.MemoryBudgetMb = 2048;
        cfg.Save();

        var cfg2 = AppConfig.Load();
        Assert.Equal("dark", cfg2.Theme);
        Assert.False(cfg2.CloseToTray);
        Assert.Equal(4, cfg2.ParallelVolumes);
        Assert.True(cfg2.DailyScan);
        Assert.Equal("04:30", cfg2.DailyScanAt);
        Assert.Equal(@"D:\SnapFindIndex", cfg2.IndexRoot);
        Assert.Equal(2048, cfg2.MemoryBudgetMb);
    }

    /// <summary>v2 移除了死配置；用户手加的**未知**键仍必须在保存时保留。</summary>
    [Fact]
    public void Save_Preserves_Unknown_Keys()
    {
        var cfg = AppConfig.Load();
        cfg.Theme = "light";
        cfg.Save();

        // 手工注入一个未来版本可能新增的键
        var json = System.Text.Json.Nodes.JsonNode.Parse(File.ReadAllText(ConfigPath))!;
        json["future_key"] = "keep-me";
        File.WriteAllText(ConfigPath, json.ToJsonString());

        var cfg2 = AppConfig.Load();
        cfg2.Theme = "dark";
        cfg2.Save();

        var json2 = System.Text.Json.Nodes.JsonNode.Parse(File.ReadAllText(ConfigPath))!;
        Assert.Equal("keep-me", json2["future_key"]!.GetValue<string>());
        Assert.Equal("dark", json2["ui"]!["theme"]!.GetValue<string>());
    }

    /// <summary>内存预算以 bytes 存（默认 1 GiB 超 int32 安全），MB 值来回转换必须一致。</summary>
    [Fact]
    public void MemoryBudget_Mb_Bytes_Roundtrip()
    {
        var cfg = AppConfig.Load();
        Assert.Equal(1024, cfg.MemoryBudgetMb); // 默认 1 GiB

        cfg.MemoryBudgetMb = 4096;
        cfg.Save();
        var json = System.Text.Json.Nodes.JsonNode.Parse(File.ReadAllText(ConfigPath))!;
        Assert.Equal(4096L * 1024 * 1024, json["index"]!["memory_budget_bytes"]!.GetValue<long>());
        Assert.Equal(4096, AppConfig.Load().MemoryBudgetMb);
    }

    [Fact]
    public void Corrupt_Config_Produces_Warning_And_Bak()
    {
        File.WriteAllText(ConfigPath, "{ this is not json ");
        var warning = "";
        AppConfig.Load(out warning);
        Assert.False(string.IsNullOrEmpty(warning), "损坏配置必须给出提示");
        Assert.True(File.Exists(ConfigPath + ".bak"), "损坏文件必须被备份");
    }
}

public class FormatTests
{
    [Theory]
    [InlineData(0, "0 B")]
    [InlineData(999, "999 B")]
    [InlineData(1024, "1.0 KB")]
    [InlineData(1024UL * 1024 * 1024, "1.0 GB")]
    public void Human_Formats_Bytes(ulong bytes, string expected) => Assert.Equal(expected, Format.Human(bytes));

    [Theory]
    [InlineData("Watching", "增量监控")]
    [InlineData("Scanning", "扫描中")]
    [InlineData("whatever", "whatever")]
    public void StateText_Maps_Known_States(string state, string expected) =>
        Assert.Equal(expected, Format.StateText(state));

    [Theory]
    [InlineData("off", "关闭")]
    [InlineData("HIGH", "高")]
    [InlineData("medium", "中")]
    [InlineData("low", "低")]
    [InlineData("", "低")]
    public void ThrottleText_Maps_Levels(string level, string expected) =>
        Assert.Equal(expected, Format.ThrottleText(level));

    [Theory]
    [InlineData("Watching", "Good")]
    [InlineData("Degraded", "Bad")]
    [InlineData("Paused", "Warn")]
    public void StateBrushKey_Maps_Colors(string state, string expected) =>
        Assert.Equal(expected, Format.StateBrushKey(state));
}

/// <summary>进度卡片状态机（IndexViewModel.ProgressRow）：不依赖 Avalonia 运行时。</summary>
public class ProgressRowTests
{
    private static Progress Progress(string state, ulong done = 10, ulong total = 100, ulong subsDone = 0, ulong subsTotal = 0) =>
        new()
        {
            Mount = "C:\\",
            State = state,
            FilesDone = done,
            FilesTotal = total,
            SubsDone = subsDone,
            SubsTotal = subsTotal,
            Throughput = new Throughput { FilesPerSec = 42 },
        };

    [Fact]
    public void Scanning_Shows_Subtree_Percent()
    {
        var row = new ProgressRow();
        row.Apply(Progress("Scanning", subsDone: 3, subsTotal: 10));
        Assert.Equal(0.3, row.Fraction, 3);
        Assert.Equal("30%", row.PercentText);
        Assert.Contains("42 文件/秒", row.StatsLine);
        Assert.False(row.IsPaused);
    }

    [Fact]
    public void Watching_Then_Stats_Shows_100_Percent()
    {
        var row = new ProgressRow();
        row.Apply(Progress("Watching", done: 100, total: 100));

        var stats = new VolumeStats
        {
            VolumeId = "v", Mount = "C:\\", State = "Watching",
            Docs = 100, IndexBytes = 1024, Segments = 3,
        };
        row.ApplyStats(stats);
        Assert.Equal(1, row.Fraction, 3);
        Assert.Equal("100%", row.PercentText);
        Assert.Contains("100 个文档", row.StatsLine);
    }

    /// <summary>R2-3：活动扫描期间 stats 快照不得覆盖实时进度（5 秒宽限窗）。</summary>
    [Fact]
    public void Stats_Does_Not_Override_Recent_Live_Progress()
    {
        var row = new ProgressRow();
        row.Apply(Progress("Scanning", subsDone: 3, subsTotal: 10));

        var stats = new VolumeStats
        {
            VolumeId = "v", Mount = "C:\\", State = "Scanning",
            Docs = 5, IndexBytes = 0, Segments = 1,
        };
        row.ApplyStats(stats); // 刚收到实时进度，必须被忽略
        Assert.Equal(0.3, row.Fraction, 3);
        Assert.Equal("30%", row.PercentText);
    }

    /// <summary>暂停时保留最后一次实时进度，进度条不得归零。</summary>
    [Fact]
    public void Paused_Keeps_Last_Fraction()
    {
        var row = new ProgressRow();
        row.Apply(Progress("Scanning", subsDone: 3, subsTotal: 10));
        row.Apply(Progress("Paused", subsDone: 3, subsTotal: 10));
        Assert.True(row.IsPaused);
        Assert.Equal(0.3, row.Fraction, 3);
    }
}

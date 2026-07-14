defmodule NopalRondoCoreLifecycleConformanceTest do
  use ExUnit.Case, async: false

  @tag timeout: 120_000
  test "concurrent Nopal callers share one detached verified Core after caller exit" do
    nopal_root = System.fetch_env!("NOPAL_ROOT")
    nopal_bin = System.fetch_env!("NOPAL_BIN")
    rondo_bin = System.fetch_env!("RONDO_BIN")
    root = temp_dir("nopal-rondo-lifecycle-conformance")
    state_root = Path.join(root, "state")
    config_root = Path.join(root, "config")
    File.mkdir_p!(config_root)

    on_exit(fn ->
      terminate_verified_fixture(nopal_bin, nopal_root, state_root, config_root, rondo_bin)
      File.rm_rf(root)
    end)

    start = fn ->
      run_nopal(
        nopal_bin,
        nopal_root,
        state_root,
        config_root,
        rondo_bin,
        ["--json", "rondo", "start", "--placement", "shared_user_runtime"]
      )
    end

    first = Task.async(start)
    second = Task.async(start)
    first = Task.await(first, 30_000)
    second = Task.await(second, 30_000)

    assert first["ok"]
    assert second["ok"]
    assert first["status"] == "running"
    assert first["placement"] == "shared_user_runtime"
    assert first["base_url"] == second["base_url"]
    assert first["instance_id"] == second["instance_id"]
    assert first["runtime_version"] == "0.1.0"
    assert first["active_run_count"] == 0

    health =
      run_nopal(
        nopal_bin,
        nopal_root,
        state_root,
        config_root,
        rondo_bin,
        ["--json", "rondo", "health"]
      )

    assert health["ok"]
    assert health["base_url"] == first["base_url"]
    assert health["instance_id"] == first["instance_id"]
    refute File.exists?(Path.join(nopal_root, ".nopal/rondo-core.json"))
    refute File.exists?(Path.join(nopal_root, ".nopal/rondo-core.log"))
  end

  defp run_nopal(nopal_bin, nopal_root, state_root, config_root, rondo_bin, args) do
    {output, status} =
      System.cmd(
        nopal_bin,
        ["--dir", nopal_root | args],
        cd: nopal_root,
        env: [
          {"NOPAL_CONFIG_DIR", config_root},
          {"NOPAL_RONDO_STATE_DIR", state_root},
          {"NOPAL_RONDO_RUNTIME", rondo_bin}
        ],
        stderr_to_stdout: true
      )

    assert status == 0, output
    Jason.decode!(output)
  end

  defp terminate_verified_fixture(nopal_bin, nopal_root, state_root, config_root, rondo_bin) do
    if File.exists?(Path.join(state_root, "runtime.json")) do
      run_nopal(
        nopal_bin,
        nopal_root,
        state_root,
        config_root,
        rondo_bin,
        ["--json", "rondo", "stop"]
      )
    end
  end

  defp temp_dir(prefix) do
    path =
      Path.join(
        System.tmp_dir!(),
        "#{prefix}-#{System.unique_integer([:positive, :monotonic])}"
      )

    File.mkdir_p!(path)
    path
  end
end

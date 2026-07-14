defmodule NopalRondoCoreBridgeConformanceTest do
  use Rondo.TestSupport

  alias Rondo.Orchestrator
  alias Rondo.RunLedger

  test "Pi submits, observes, and deduplicates one approved run through actual Rondo Core" do
    nopal_root = System.fetch_env!("NOPAL_ROOT")
    nopal_bin = System.fetch_env!("NOPAL_BIN")
    run_events_schema = System.fetch_env!("NOPAL_RUN_EVENTS_SCHEMA")
    schema_validator = System.fetch_env!("NOPAL_SCHEMA_VALIDATOR")
    root = temp_dir("nopal-rondo-core-conformance")
    workspace_root = Path.join(root, "rondo-workspaces")
    repo_id = "nopal-conformance-repo"
    plot_id = "plot-conformance"
    nopal_state = Path.join(root, "nopal-state")
    parent = self()
    previous_service_mode = Application.get_env(:rondo, :service_mode)

    Application.put_env(:rondo, :service_mode, :trackerless_core)

    on_exit(fn ->
      if is_nil(previous_service_mode) do
        Application.delete_env(:rondo, :service_mode)
      else
        Application.put_env(:rondo, :service_mode, previous_service_mode)
      end

      File.rm_rf(root)
    end)

    write_workflow_file!(Workflow.workflow_file_path(),
      tracker_kind: "memory",
      workspace_root: workspace_root,
      max_concurrent_agents: 1,
      poll_interval_ms: 60_000,
      observability_enabled: false
    )

    task_supervisor = unique_name(:ConformanceTaskSupervisor)
    orchestrator = unique_name(:ConformanceOrchestrator)

    start_supervised!(
      Supervisor.child_spec(
        {Task.Supervisor, name: task_supervisor},
        id: task_supervisor
      )
    )

    runner = fn issue, recipient, opts ->
      runner_pid = self()
      send(parent, {:conformance_runner_started, runner_pid, issue, recipient, opts})

      receive do
        {:release_conformance_runner, ^runner_pid} -> :ok
      end
    end

    start_supervised!(
      Supervisor.child_spec(
        {Orchestrator,
         name: orchestrator,
         task_supervisor: task_supervisor,
         execution_request_runner: runner,
         service_mode: :trackerless_core,
         tracker_polling: false},
        id: orchestrator
      )
    )

    http_server_id = unique_name(:ConformanceHttpServer)

    start_supervised!(
      Supervisor.child_spec(
        {Rondo.HttpServer, port: 0, host: "127.0.0.1", orchestrator: orchestrator},
        id: http_server_id
      )
    )

    port = eventually(fn -> Rondo.HttpServer.bound_port() end)
    base_url = "http://127.0.0.1:#{port}"
    manifest_path = write_fixture(root, nopal_state, base_url, repo_id, plot_id)
    config_dir = Path.join(root, "isolated-config")
    File.mkdir_p!(config_dir)

    node_script =
      Path.join(
        nopal_root,
        "tests/conformance/pi-rondo-core.mjs"
      )

    {node_output, 0} =
      System.cmd(
        "node",
        ["--no-warnings", node_script, root, manifest_path, plot_id, "start"],
        cd: nopal_root,
        env: [
          {"NOPAL_BIN", nopal_bin},
          {"NOPAL_CONFIG_DIR", config_dir},
          {"NOPAL_STATE_DIR", nopal_state},
          {"NOPAL_RONDO_CORE_URL", "http://127.0.0.1:1"}
        ],
        stderr_to_stdout: false
      )

    %{"start" => started} = Jason.decode!(node_output)
    assert started["kind"] == "nopal.run_submit/v1"
    assert started["ok"]
    refute started["deduplicated"]
    assert started["handle"]["repo_id"] == repo_id
    assert started["handle"]["plot_id"] == plot_id
    assert started["handle"]["event_cursor"] == "rondo.core/v1:0"
    submitted_plot = read_plot!(nopal_state, plot_id)
    assert submitted_plot["selected_session_id"] == "session-conformance"
    assert [submitted_execution] = submitted_plot["executions"]
    assert submitted_execution["service_id"] == "rondo-core"
    assert submitted_execution["repo_id"] == repo_id
    assert submitted_execution["run_id"] == started["handle"]["run_id"]
    assert submitted_execution["status"] == "running"
    assert is_nil(submitted_execution["outcome"])
    assert_receive {:conformance_runner_started, runner_pid, issue, recipient, runner_opts}, 1_000
    assert Process.alive?(runner_pid), "the Rondo worker must outlive the Nopal submit process"
    assert recipient == Process.whereis(orchestrator)
    assert "execution-request:" <> identity = issue.id
    assert issue.identifier == "execution-request-#{identity}"
    assert issue.title == "Execution request rondo-core-conformance"
    assert issue.description =~ "Return successfully without modifying the repository."
    assert runner_opts[:attempt] == 0
    assert runner_opts[:trackerless] == true
    assert is_nil(runner_opts[:worker_host])
    assert %RunLedger{} = runner_opts[:run_ledger]
    assert runner_opts[:run_dir] == runner_opts[:run_ledger].run_dir

    frozen_source_contract = runner_opts[:source_contract]
    manifest_bytes = File.read!(manifest_path)
    manifest_sha256 = Base.encode16(:crypto.hash(:sha256, manifest_bytes), case: :lower)
    assert frozen_source_contract.schema == "approved-slice-v1"
    assert frozen_source_contract.slice_id == "rondo-core-conformance"
    assert frozen_source_contract.sha256 == manifest_sha256
    assert {:ok, canonical_manifest_path} = Rondo.PathSafety.canonicalize(manifest_path)
    assert frozen_source_contract.source_path == canonical_manifest_path
    assert frozen_source_contract.path == Path.join(runner_opts[:run_dir], "artifacts/execution-request.json")
    assert File.read!(frozen_source_contract.path) == manifest_bytes

    assert File.read!(Path.join(runner_opts[:run_dir], "artifacts/approval-bundle.json")) ==
             File.read!(Path.join(Path.dirname(Path.dirname(manifest_path)), "bundle.json"))

    send(runner_pid, {:release_conformance_runner, runner_pid})

    {result_output, 0} =
      System.cmd(
        "node",
        [
          "--no-warnings",
          node_script,
          root,
          manifest_path,
          plot_id,
          "result",
          repo_id,
          started["handle"]["run_id"],
          started["handle"]["event_cursor"]
        ],
        cd: nopal_root,
        env: [
          {"NOPAL_BIN", nopal_bin},
          {"NOPAL_CONFIG_DIR", config_dir},
          {"NOPAL_STATE_DIR", nopal_state},
          {"NOPAL_RONDO_CORE_URL", "http://127.0.0.1:1"}
        ],
        stderr_to_stdout: false
      )

    %{"result" => result} = Jason.decode!(result_output)

    assert result["kind"] == "nopal.afk_result/v1"
    assert result["ok"]
    assert result["outcome"] == "settled"
    assert result["status"] == "completed"
    assert result["settled"]
    refute result["has_more"]
    completed_plot = read_plot!(nopal_state, plot_id)
    assert completed_plot["selected_session_id"] == "session-conformance"
    assert [completed_execution] = completed_plot["executions"]
    assert completed_execution["run_id"] == started["handle"]["run_id"]
    assert completed_execution["status"] == "completed"
    assert completed_execution["outcome"] == "completed"
    assert completed_execution["event_cursor"] == result["next_event_cursor"]
    assert completed_execution["evidence"] != []

    assert result["handle"] == %{
             "repo_id" => repo_id,
             "plot_id" => plot_id,
             "run_id" => started["handle"]["run_id"]
           }

    run_id = started["handle"]["run_id"]
    assert cursor_offset(result["next_event_cursor"]) == length(result["events"])
    assert opaque_evidence?(result["evidence_pointers"], root)

    archived = fetch_events!(base_url, repo_id, run_id, "rondo.core/v1:0")
    archived_replay = fetch_events!(base_url, repo_id, run_id, "rondo.core/v1:0")
    assert archived_replay == archived
    assert archived["events"] != [], "a completed real run must expose a non-empty Core event feed"
    assert archived["events"] == result["events"]
    assert archived["plot_id"] == plot_id
    assert archived["next_event_cursor"] == result["next_event_cursor"]
    refute archived["has_more"]
    assert_live_event_contract!(archived, root, plot_id)

    resume_offset = 2
    assert length(archived["events"]) > resume_offset
    resumed = fetch_events!(base_url, repo_id, run_id, "rondo.core/v1:#{resume_offset}")
    assert resumed["events"] == Enum.drop(archived["events"], resume_offset)
    assert resumed["next_event_cursor"] == archived["next_event_cursor"]
    assert resumed["has_more"] == archived["has_more"]

    tail = fetch_events!(base_url, repo_id, run_id, archived["next_event_cursor"])
    assert tail["events"] == []
    assert tail["next_event_cursor"] == archived["next_event_cursor"]
    refute tail["has_more"]

    validate_live_response_shapes!(
      root,
      schema_validator,
      run_events_schema,
      [archived, archived_replay, resumed, tail]
    )

    assert :ok = stop_supervised(http_server_id)
    assert :ok = stop_supervised(orchestrator)

    restart_runner = fn _issue, _recipient, _opts ->
      raise "a terminal durable Core run must not dispatch again after restart"
    end

    start_supervised!(
      Supervisor.child_spec(
        {Orchestrator,
         name: orchestrator,
         task_supervisor: task_supervisor,
         execution_request_runner: restart_runner,
         service_mode: :trackerless_core,
         tracker_polling: false},
        id: orchestrator
      )
    )

    start_supervised!(
      Supervisor.child_spec(
        {Rondo.HttpServer, port: port, host: "127.0.0.1", orchestrator: orchestrator},
        id: http_server_id
      )
    )

    assert eventually(fn -> Rondo.HttpServer.bound_port() end) == port

    {restart_output, 0} =
      System.cmd(
        nopal_bin,
        [
          "--dir",
          root,
          "--json",
          "run",
          "observe",
          "--repo-id",
          repo_id,
          "--plot-id",
          plot_id,
          "--run-id",
          started["handle"]["run_id"],
          "--state-dir",
          nopal_state
        ],
        env: [
          {"NOPAL_CONFIG_DIR", config_dir},
          {"NOPAL_STATE_DIR", nopal_state},
          {"NOPAL_RONDO_CORE_URL", nil}
        ]
      )

    restart_observation = Jason.decode!(restart_output)
    assert restart_observation["ok"]
    assert restart_observation["status"] == "completed"
    assert restart_observation["next_event_cursor"] == result["next_event_cursor"]
    assert restart_observation["events"] == []
    assert read_plot!(nopal_state, plot_id) == completed_plot

    {field_output, 0} =
      System.cmd(
        nopal_bin,
        [
          "--dir",
          root,
          "field",
          "inspect",
          "--all",
          "--json",
          "--state-dir",
          nopal_state
        ],
        env: [
          {"NOPAL_CONFIG_DIR", config_dir},
          {"NOPAL_STATE_DIR", nopal_state}
        ]
      )

    field = Jason.decode!(field_output)
    assert field["kind"] == "nopal.field/v1"
    assert [projected_plot] = field["plots"]
    assert projected_plot["plot_id"] == plot_id
    assert projected_plot["sessions"] == completed_plot["sessions"]
    assert projected_plot["selected_session_id"] == completed_plot["selected_session_id"]
    assert projected_plot["executions"] == completed_plot["executions"]
    assert hd(projected_plot["executions"])["evidence"] == completed_execution["evidence"]

    {replay_output, 0} =
      System.cmd(
        nopal_bin,
        [
          "--dir",
          root,
          "--json",
          "run",
          "submit",
          "--manifest",
          manifest_path,
          "--plot-id",
          plot_id,
          "--state-dir",
          nopal_state
        ],
        env: [
          {"NOPAL_CONFIG_DIR", config_dir},
          {"NOPAL_STATE_DIR", nopal_state},
          {"NOPAL_RONDO_CORE_URL", nil}
        ]
      )

    replay = Jason.decode!(replay_output)
    assert replay["ok"]
    assert replay["deduplicated"]
    assert replay["handle"]["repo_id"] == repo_id
    assert replay["handle"]["plot_id"] == plot_id
    assert replay["handle"]["run_id"] == started["handle"]["run_id"]
    assert read_plot!(nopal_state, plot_id)["executions"] == completed_plot["executions"]
    refute_receive {:conformance_runner_started, _runner_pid, _issue, _recipient, _opts}, 200

    manifests =
      Path.wildcard(
        Path.join([
          workspace_root,
          ".rondo_runs",
          "*",
          "*",
          "manifest.json"
        ])
      )

    accepted =
      Enum.filter(manifests, fn path ->
        manifest = path |> File.read!() |> Jason.decode!()

        manifest["source"] == "execution_request" and
          get_in(manifest, ["admission", "phase"]) == "accepted"
      end)

    assert [accepted_manifest_path] = accepted

    assert {:ok, accepted_manifest} =
             accepted_manifest_path
             |> Path.dirname()
             |> RunLedger.open_run()
             |> then(fn {:ok, ledger} -> RunLedger.load_manifest(ledger.run_dir) end)

    assert accepted_manifest["run_id"] == started["handle"]["run_id"]
    assert get_in(accepted_manifest, ["admission", "plot_id"]) == plot_id
  end

  defp write_fixture(root, nopal_state, base_url, repo_id, plot_id) do
    File.mkdir_p!(Path.join(root, ".nopal"))

    File.write!(
      Path.join(root, ".nopal/nopal.jsonc"),
      ~s({"version":"nopal.project/v1","project":{"name":"core-conformance"},"profile":"portable"}\n)
    )

    plots_dir = Path.join(nopal_state, "plots")
    File.mkdir_p!(plots_dir)

    File.write!(
      Path.join(plots_dir, "#{plot_id}.json"),
      Jason.encode!(%{
        kind: "nopal.plot/v1",
        plot_id: plot_id,
        title: "Conformance Plot",
        provisional: false,
        progress: "planned",
        conditions: [],
        seed: %{source: "conformance", text: "Cross-repository Plot correlation"},
        intent: "Prove Nopal and Rondo preserve explicit Plot identity",
        sessions: [
          %{
            session_id: "session-conformance",
            mode: "interactive",
            host: "pi",
            host_session: "nopal-conformance",
            host_pane: "%1",
            state: "active",
            workspace: nil,
            created_at: "2026-07-12T00:00:00Z",
            updated_at: "2026-07-12T00:00:00Z"
          }
        ],
        selected_session_id: "session-conformance",
        establishment: %{
          event: "kickoff_context_ready",
          primary_repository_id: repo_id,
          effective_workflow: %{
            source_repository_id: repo_id,
            source_hash: String.duplicate("a", 64),
            value: %{}
          },
          applied_requests: [],
          established_at: "2026-07-12T00:00:00Z"
        },
        repositories: [],
        workspaces: [],
        created_at: "2026-07-12T00:00:00Z",
        updated_at: "2026-07-12T00:00:00Z"
      })
    )

    File.write!(
      Path.join(root, ".nopal/gates.jsonc"),
      ~s({"version":"nopal.gates/v1","gates":[]}\n)
    )

    File.write!(
      Path.join(root, ".nopal/policy.jsonc"),
      ~s({"version":"nopal.policy/v1","modes":{"nopal_tui":{"default_decision":"allow","default_placement":"dedicated_run_runtime","rules":[]}}}\n)
    )

    File.write!(
      Path.join(root, ".nopal/config.jsonc"),
      Jason.encode!(%{
        version: "nopal.config/v1",
        rondo_core: %{
          base_url: base_url,
          request_timeout_ms: 5_000,
          repo_id: repo_id
        }
      }) <> "\n"
    )

    bundle_dir =
      Path.join([
        root,
        ".beislid",
        "exports",
        "rondo-core-conformance"
      ])

    slices_dir = Path.join(bundle_dir, "slices")
    File.mkdir_p!(slices_dir)
    slice_id = "rondo-core-conformance"

    manifest = %{
      schema: "approved-slice-v1",
      slice_id: slice_id,
      prompt: "Return successfully without modifying the repository.",
      repo: %{
        url: "https://example.test/harmless.git",
        base_ref: "main",
        base_sha: String.duplicate("a", 40)
      }
    }

    manifest_path = Path.join(slices_dir, "#{slice_id}.json")
    File.write!(manifest_path, Jason.encode!(manifest))
    File.write!(Path.join(slices_dir, "#{slice_id}.md"), "# Harmless Core conformance\n")

    bundle = %{
      kind: "approved-slice-plan-export-v0",
      version: 1,
      status: "approved",
      generated_from: "cross-repository-conformance",
      source_work_contract: "cross-repository-conformance",
      slice_plan: %{},
      children: [%{id: slice_id}],
      dependency_graph: %{slice_id => []},
      proof_requirements: [],
      guides_and_gates: %{},
      approval: %{
        approved_at: "2026-07-10T00:00:00Z",
        approved_by: "Nopal Rondo Core conformance",
        verdicts: %{slice_id => "approve"}
      },
      runner_extensions: %{},
      validation: %{
        schema_version: "approved-slice-plan-export-v0",
        rubric_version: "afk-rubric-v1"
      },
      ownership: %{},
      supersedes: nil
    }

    File.write!(Path.join(bundle_dir, "bundle.json"), Jason.encode!(bundle))
    manifest_path
  end

  defp read_plot!(state_dir, plot_id) do
    state_dir
    |> Path.join("plots/#{plot_id}.json")
    |> File.read!()
    |> Jason.decode!()
  end

  defp opaque_evidence?(pointers, root) when is_list(pointers) and pointers != [] do
    Enum.all?(pointers, fn pointer ->
      uri = Map.get(pointer, "uri")

      is_binary(uri) and String.starts_with?(uri, "rondo-run://") and
        not String.contains?(uri, root)
    end)
  end

  defp opaque_evidence?(_pointers, _root), do: false

  defp fetch_events!(base_url, repo_id, run_id, cursor) do
    encoded_run_id = URI.encode(run_id, &URI.char_unreserved?/1)

    response =
      Req.get!(
        "#{base_url}/api/v1/runs/#{encoded_run_id}/events",
        params: %{"repo_id" => repo_id, "cursor" => cursor},
        retry: false
      )

    assert response.status == 200
    assert is_map(response.body)
    response.body
  end

  defp assert_live_event_contract!(page, root, plot_id) do
    assert page["plot_id"] == plot_id
    events = page["events"]
    assert is_list(events)
    assert events != [], "a completed real run must expose a non-empty Core event feed"

    expected_families =
      MapSet.new([
        "rondo.service.status_changed",
        "rondo.run.status_changed",
        "rondo.run.evidence_recorded"
      ])

    assert events |> Enum.map(& &1["type"]) |> MapSet.new() == expected_families
    assert Enum.map(events, & &1["sequence"]) == Enum.to_list(1..length(events))
    assert cursor_offset(page["next_event_cursor"]) == length(events)

    Enum.each(events, fn event ->
      if event["type"] != "rondo.service.status_changed" do
        assert event["plot_id"] == plot_id
        assert get_in(event, ["namespace", "plot_id"]) == plot_id
      end
    end)

    evidence_events =
      Enum.filter(events, &(&1["type"] == "rondo.run.evidence_recorded"))

    assert evidence_events != []

    Enum.each(evidence_events, fn event ->
      uri = event["uri"]
      assert is_binary(uri)
      assert String.starts_with?(uri, "rondo-run://")
      refute String.contains?(uri, root)
      refute String.starts_with?(uri, "file://")
    end)
  end

  defp validate_live_response_shapes!(root, validator, schema, pages) do
    observations_dir = Path.join(root, "core-observations")
    File.mkdir_p!(observations_dir)

    paths =
      pages
      |> Enum.with_index()
      |> Enum.map(fn {page, index} ->
        path = Path.join(observations_dir, "run-events-#{index}.json")
        File.write!(path, Jason.encode!(page))
        path
      end)

    {output, status} =
      System.cmd(
        "node",
        [validator, "documents", schema | paths],
        stderr_to_stdout: true
      )

    assert status == 0, output
  end

  defp eventually(fun, attempts \\ 100)
  defp eventually(_fun, 0), do: flunk("condition did not become true")

  defp eventually(fun, attempts) do
    case fun.() do
      nil ->
        Process.sleep(20)
        eventually(fun, attempts - 1)

      false ->
        Process.sleep(20)
        eventually(fun, attempts - 1)

      value ->
        value
    end
  end

  defp cursor_offset("rondo.core/v1:" <> encoded), do: String.to_integer(encoded)

  defp unique_name(suffix) do
    Module.concat(
      __MODULE__,
      "#{suffix}_#{System.unique_integer([:positive])}"
    )
  end

  defp temp_dir(prefix) do
    path =
      Path.join(
        System.tmp_dir!(),
        "#{prefix}-#{System.unique_integer([:positive])}"
      )

    File.mkdir_p!(path)
    path
  end
end

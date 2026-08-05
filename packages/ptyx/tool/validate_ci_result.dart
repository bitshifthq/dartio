import 'dart:convert';
import 'dart:io';

/// Workloads required from the CI invocation of the diagnostic scorecard.
const scorecardWorkloads = {
  'interactive',
  'output',
  'transport_output',
  'input',
  'transport_input',
  'bidirectional',
  'pause_resume',
  'discard',
  'no_listener',
  'saturation',
  'fairness',
  'active_output',
  'spawn_close',
  'observation',
  'forced_close',
  'idle_1',
  'idle_10',
  'idle_100',
};

const _repeatedWorkloads = {
  'output',
  'transport_output',
  'input',
  'transport_input',
  'bidirectional',
  'pause_resume',
  'discard',
};
const _minimumQualificationBytes = 128 * 1024 * 1024;

Future<void> main(List<String> arguments) async {
  if (arguments.length != 2 ||
      !const {'scorecard', 'soak'}.contains(arguments[0])) {
    stderr.writeln(
      'usage: dart run tool/validate_ci_result.dart <scorecard|soak> <path>',
    );
    exitCode = 64;
    return;
  }

  final file = File(arguments[1]);
  if (!file.existsSync()) {
    stderr.writeln('CI result does not exist: ${file.path}');
    exitCode = 1;
    return;
  }

  Object? decoded;
  try {
    decoded = jsonDecode(await file.readAsString());
  } on FormatException catch (error) {
    stderr.writeln('CI result is not valid JSON: $error');
    exitCode = 1;
    return;
  }
  if (decoded is! Map<String, Object?>) {
    stderr.writeln('CI result must be a JSON object');
    exitCode = 1;
    return;
  }

  final failures = validateCiResult(arguments[0], decoded);
  final expectedRevision = Platform.environment['GITHUB_SHA'];
  if (expectedRevision != null &&
      expectedRevision.isNotEmpty &&
      decoded['revision'] != expectedRevision) {
    failures.add(
      '${arguments[0]} revision ${decoded['revision']} does not match '
      'GITHUB_SHA $expectedRevision',
    );
  }
  final expectedPlatform = Platform.environment['RUNNER_OS']?.toLowerCase();
  if (expectedPlatform != null &&
      expectedPlatform.isNotEmpty &&
      decoded['platform'] != expectedPlatform) {
    failures.add(
      '${arguments[0]} platform ${decoded['platform']} does not match '
      'RUNNER_OS $expectedPlatform',
    );
  }
  final expectedArchitecture = canonicalArchitecture(
    Platform.environment['RUNNER_ARCH'],
  );
  final actualArchitecture = canonicalArchitecture(
    '${decoded['architecture']}',
  );
  if (expectedArchitecture != null &&
      expectedArchitecture.isNotEmpty &&
      actualArchitecture != expectedArchitecture) {
    failures.add(
      '${arguments[0]} architecture ${decoded['architecture']} does not match '
      'RUNNER_ARCH $expectedArchitecture',
    );
  }
  if (failures.isNotEmpty) {
    for (final failure in failures) {
      stderr.writeln(failure);
    }
    exitCode = 1;
    return;
  }
  stdout.writeln('validated ${arguments[0]} CI result: ${file.path}');
}

/// Normalizes common OS and CI architecture spellings.
String? canonicalArchitecture(String? value) => switch (value?.toLowerCase()) {
  'x64' || 'x86_64' || 'amd64' => 'x64',
  'arm64' || 'aarch64' => 'arm64',
  'x86' || 'i386' || 'i686' => 'x86',
  'arm' || 'armv7' || 'armv7l' => 'arm',
  final value? when value.isNotEmpty => value,
  _ => null,
};

/// Returns structural or completion failures for a CI [artifact] of [kind].
List<String> validateCiResult(String kind, Map<String, Object?> artifact) {
  final failures = <String>[];
  final (schema, suite) = switch (kind) {
    'scorecard' => (4, 'ptyx-diagnostic-scorecard'),
    'soak' => (1, 'ptyx-exact-integrity-soak'),
    _ => throw ArgumentError.value(kind, 'kind', 'unsupported CI result kind'),
  };
  if (artifact['schema'] != schema) {
    failures.add('$kind schema must be $schema');
  }
  if (artifact['suite'] != suite) {
    failures.add('$kind suite must be $suite');
  }
  if (artifact['revision'] is! String ||
      (artifact['revision']! as String).isEmpty) {
    failures.add('$kind revision must be non-empty');
  }
  if (artifact['tree_dirty'] != false) {
    failures.add('$kind tree_dirty must be false');
  }
  if (artifact['platform'] is! String ||
      (artifact['platform']! as String).isEmpty) {
    failures.add('$kind platform must be non-empty');
  }
  if (artifact['architecture'] is! String ||
      (artifact['architecture']! as String).isEmpty) {
    failures.add('$kind architecture must be non-empty');
  }

  switch (kind) {
    case 'scorecard':
      for (final workload in scorecardWorkloads) {
        final result = artifact[workload];
        if (result is! Map<String, Object?>) {
          failures.add('scorecard is missing $workload');
          continue;
        }
        if (_repeatedWorkloads.contains(workload)) {
          final runs = result['raw_runs'];
          final repetitions = result['repetitions'];
          final warmups = result['warmups'];
          if (runs is! List<Object?> ||
              repetitions is! int ||
              repetitions < 3 ||
              warmups is! int ||
              warmups < 1 ||
              runs.length != repetitions) {
            failures.add(
              '$workload must contain at least three post-warmup raw runs',
            );
            continue;
          }
          for (final (index, run) in runs.indexed) {
            if (run is! Map<String, Object?>) {
              failures.add('$workload run $index is not an object');
              continue;
            }
            final exitCode = run['exit_code'];
            if (exitCode != 0) {
              failures.add('$workload run $index exited with $exitCode');
            }
            if (!_validRepeatedRun(workload, run, '${artifact['platform']}')) {
              failures.add('$workload run $index is missing required metrics');
            }
          }
          final distribution = result['distribution'];
          if (result['metric'] is! String ||
              distribution is! Map<String, Object?> ||
              distribution['samples'] is! int ||
              (distribution['samples']! as int) <= 0) {
            failures.add('$workload is missing its metric distribution');
          }
        }
      }
      final interactive = artifact['interactive'];
      if (interactive is Map<String, Object?> &&
          (interactive['samples'] is! int ||
              (interactive['samples']! as int) <= 0 ||
              interactive['p99_us'] is! num)) {
        failures.add('interactive must contain latency samples');
      }
      for (final workload in const ['no_listener', 'saturation']) {
        final result = artifact[workload];
        if (result is Map<String, Object?> && result['exit_code'] != 0) {
          failures.add('$workload exited with ${result['exit_code']}');
        }
      }
      for (final count in const [1, 10, 100]) {
        final name = 'idle_$count';
        final result = artifact[name];
        if (result is Map<String, Object?> &&
            (result['requested_sessions'] != count ||
                result['created_sessions'] != count ||
                result['failure'] != null)) {
          failures.add(
            '$name created ${result['created_sessions']} of $count sessions',
          );
        }
      }
      final forcedClose = artifact['forced_close'];
      if (forcedClose is Map<String, Object?> &&
          (forcedClose['descendant_reclaimed'] != true ||
              forcedClose['exit_code'] is! int)) {
        failures.add('forced_close did not prove exit and descendant cleanup');
      }
      final fairness = artifact['fairness'];
      if (fairness is Map<String, Object?> &&
          (fairness['sessions'] != 16 ||
              fairness['round_trips_per_session'] is! int ||
              fairness['per_session'] is! List<Object?> ||
              !_validFairness(fairness['per_session']! as List<Object?>))) {
        failures.add('fairness did not exercise 16 sessions');
      }
      final activeOutput = artifact['active_output'];
      if (activeOutput is Map<String, Object?> &&
          (activeOutput['sessions'] != 16 ||
              activeOutput['per_session'] is! List<Object?> ||
              !_validActiveOutput(
                activeOutput['per_session']! as List<Object?>,
              ) ||
              activeOutput['slowest_to_fastest_ratio'] is! num)) {
        failures.add('active_output did not exercise 16 sessions');
      }
      final spawnClose = artifact['spawn_close'];
      if (spawnClose is Map<String, Object?> &&
          (spawnClose['samples'] is! int ||
              (spawnClose['samples']! as int) <= 0)) {
        failures.add('spawn_close must contain latency samples');
      }
      final observation = artifact['observation'];
      if (observation is Map<String, Object?> &&
          (observation['repetitions'] is! int ||
              (observation['repetitions']! as int) <= 0 ||
              observation['mode_samples'] is! int)) {
        failures.add('observation must contain resize and mode samples');
      }
      if (artifact['rss_after_bytes'] is! int ||
          (artifact['rss_after_bytes']! as int) <= 0) {
        failures.add('scorecard is missing rss_after_bytes');
      }
    case 'soak':
      if (artifact['cycles'] is! int || (artifact['cycles']! as int) <= 0) {
        failures.add('soak must complete at least one cycle');
      }
      if (artifact['verified_bytes'] is! int ||
          (artifact['verified_bytes']! as int) <= 0 ||
          artifact['long_lived_verified_bytes'] is! int ||
          (artifact['long_lived_verified_bytes']! as int) <= 0) {
        failures.add('soak must verify finite and long-lived traffic');
      }
      if (artifact['cleanup_passed'] != true) {
        failures.add('soak cleanup did not pass');
      }
      if (artifact['passed'] != true ||
          artifact['resource_counts_stabilized'] != true ||
          artifact['threads_within_growth_budget'] != true ||
          artifact['resource_units_within_growth_budget'] != true) {
        failures.add('soak resource and completion gates did not pass');
      }
      for (final snapshotName in const ['resource_before', 'resource_after']) {
        final snapshot = artifact[snapshotName];
        if (snapshot is! Map<String, Object?> ||
            snapshot['tree_cpu_us'] is! int ||
            (snapshot['tree_cpu_us']! as int) < 0) {
          failures.add('soak $snapshotName must include CPU accounting');
        }
      }
  }
  return failures;
}

bool _validRepeatedRun(
  String workload,
  Map<String, Object?> run,
  String platform,
) {
  bool positive(String key) => run[key] is num && (run[key]! as num) > 0;
  final hasQualificationBytes = switch (workload) {
    'output' || 'transport_output' || 'input' || 'transport_input' =>
      run['bytes'] is int &&
          (run['bytes']! as int) >= _minimumQualificationBytes,
    'bidirectional' =>
      run['sent_bytes'] is int &&
          (run['sent_bytes']! as int) >= _minimumQualificationBytes,
    'pause_resume' =>
      run['bytes'] is int &&
          (run['bytes']! as int) >= _minimumQualificationBytes,
    'discard' =>
      run['generated_bytes'] is int &&
          (run['generated_bytes']! as int) >= _minimumQualificationBytes,
    _ => false,
  };
  return switch (workload) {
    'output' || 'transport_output' || 'input' || 'transport_input' =>
      hasQualificationBytes &&
          positive('bytes') &&
          positive('elapsed_us') &&
          positive('mib_per_second') &&
          (platform != 'windows' || run['trailing_bytes'] == 0),
    'bidirectional' =>
      hasQualificationBytes &&
          positive('sent_bytes') &&
          _validBidirectionalReceipt(run) &&
          positive('elapsed_us') &&
          positive('aggregate_mib_per_second'),
    'pause_resume' =>
      hasQualificationBytes &&
          positive('bytes') &&
          positive('pause_ms') &&
          positive('resume_to_eof_us') &&
          (platform != 'windows' || run['trailing_bytes'] == 0),
    'discard' =>
      hasQualificationBytes &&
          positive('generated_bytes') &&
          positive('elapsed_us'),
    _ => false,
  };
}

bool _validBidirectionalReceipt(Map<String, Object?> run) {
  final sent = run['sent_bytes'];
  final received = run['received_bytes'];
  if (sent is! int || received is! int) return false;
  return received == sent &&
      run['trailing_bytes'] == 0 &&
      run['integrity_scope'] is String &&
      (run['integrity_scope']! as String).isNotEmpty;
}

bool _validFairness(List<Object?> sessions) {
  if (sessions.length != 16) return false;
  final identities = <int>{};
  for (final value in sessions) {
    if (value is! Map<String, Object?> ||
        value['session'] is! int ||
        value['samples'] is! int ||
        (value['samples']! as int) <= 0 ||
        value['p99_us'] is! num) {
      return false;
    }
    identities.add(value['session']! as int);
  }
  return identities.length == 16 &&
      identities.every((identity) => identity >= 0 && identity < 16);
}

bool _validActiveOutput(List<Object?> sessions) {
  if (sessions.length != 16) return false;
  final identities = <int>{};
  for (final value in sessions) {
    if (value is! Map<String, Object?> ||
        value['session'] is! int ||
        value['elapsed_us'] is! num ||
        (value['elapsed_us']! as num) <= 0 ||
        value['mib_per_second'] is! num ||
        (value['mib_per_second']! as num) <= 0) {
      return false;
    }
    identities.add(value['session']! as int);
  }
  return identities.length == 16 &&
      identities.every((identity) => identity >= 0 && identity < 16);
}

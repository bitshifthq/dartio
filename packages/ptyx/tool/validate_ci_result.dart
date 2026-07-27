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
  if (failures.isNotEmpty) {
    for (final failure in failures) {
      stderr.writeln(failure);
    }
    exitCode = 1;
    return;
  }
  stdout.writeln('validated ${arguments[0]} CI result: ${file.path}');
}

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
  if (artifact['tree_dirty'] is! bool) {
    failures.add('$kind tree_dirty must be boolean');
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
        if (artifact[workload] is! Map<String, Object?>) {
          failures.add('scorecard is missing $workload');
        }
      }
      if (artifact['rss_after_bytes'] is! int) {
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
  }
  return failures;
}

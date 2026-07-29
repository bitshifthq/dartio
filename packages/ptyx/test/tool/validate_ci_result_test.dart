import 'package:test/test.dart';

import '../../tool/validate_ci_result.dart';

void main() {
  test('canonicalArchitecture normalizes producer and CI aliases', () {
    expect(canonicalArchitecture('X64'), 'x64');
    expect(canonicalArchitecture('x86_64'), 'x64');
    expect(canonicalArchitecture('AMD64'), 'x64');
    expect(canonicalArchitecture('ARM64'), 'arm64');
    expect(canonicalArchitecture('aarch64'), 'arm64');
  });

  group('validateCiResult', () {
    group('scorecard', () {
      test('accepts a complete scorecard artifact', () {
        final artifact = <String, Object?>{
          ..._metadata(schema: 4, suite: 'ptyx-diagnostic-scorecard'),
          for (final workload in scorecardWorkloads)
            workload: _scorecardWorkload(workload),
          'rss_after_bytes': 1,
        };

        final failures = validateCiResult('scorecard', artifact);

        expect(failures, isEmpty);
      });

      test('rejects a scorecard missing a workload', () {
        final artifact = <String, Object?>{
          ..._metadata(schema: 4, suite: 'ptyx-diagnostic-scorecard'),
          'rss_after_bytes': 1,
        };

        final failures = validateCiResult('scorecard', artifact);

        expect(failures, contains('scorecard is missing interactive'));
      });

      test('rejects partial idle-session and failed workload results', () {
        final artifact = <String, Object?>{
          ..._metadata(schema: 4, suite: 'ptyx-diagnostic-scorecard'),
          for (final workload in scorecardWorkloads)
            workload: <String, Object?>{},
          'output': {
            'raw_runs': [
              {'bytes': 1, 'exit_code': 7},
            ],
          },
          'no_listener': {'exit_code': 7},
          'idle_1': _idleResult(1),
          'idle_10': _idleResult(10),
          'idle_100': {
            'requested_sessions': 100,
            'created_sessions': 99,
            'failure': 'spawn failed',
          },
          'rss_after_bytes': 1,
        };

        final failures = validateCiResult('scorecard', artifact);

        expect(failures, contains('output run 0 exited with 7'));
        expect(
          failures,
          contains('transport_output must contain at least one raw run'),
        );
        expect(failures, contains('no_listener exited with 7'));
        expect(failures, contains('idle_100 created 99 of 100 sessions'));
      });

      test('rejects zero bidirectional receipt outside Windows', () {
        final artifact = <String, Object?>{
          ..._metadata(schema: 4, suite: 'ptyx-diagnostic-scorecard'),
          'platform': 'linux',
          for (final workload in scorecardWorkloads)
            workload: _scorecardWorkload(workload),
          'bidirectional': {
            'metric': 'fixture',
            'distribution': {'samples': 1},
            'raw_runs': [
              {..._repeatedRun('bidirectional'), 'received_bytes': 0},
            ],
          },
          'rss_after_bytes': 1,
        };

        final failures = validateCiResult('scorecard', artifact);

        expect(
          failures,
          contains('bidirectional run 0 is missing required metrics'),
        );
      });
    });

    group('soak', () {
      test('rejects a result without successful cleanup', () {
        final artifact = <String, Object?>{
          ..._metadata(schema: 1, suite: 'ptyx-exact-integrity-soak'),
          'cycles': 1,
          'verified_bytes': 1,
          'long_lived_verified_bytes': 1,
          'cleanup_passed': false,
        };

        final failures = validateCiResult('soak', artifact);

        expect(failures, contains('soak cleanup did not pass'));
      });
    });
  });
}

Map<String, Object?> _idleResult(int count) => {
  'requested_sessions': count,
  'created_sessions': count,
  'failure': null,
};

Map<String, Object?> _scorecardWorkload(String workload) => switch (workload) {
  'output' ||
  'transport_output' ||
  'input' ||
  'transport_input' ||
  'bidirectional' ||
  'pause_resume' ||
  'discard' => {
    'metric': 'fixture',
    'distribution': {'samples': 1},
    'raw_runs': [_repeatedRun(workload)],
  },
  'interactive' => {'samples': 1, 'p99_us': 1},
  'no_listener' || 'saturation' => {'exit_code': 0},
  'fairness' => {
    'sessions': 16,
    'round_trips_per_session': 1,
    'per_session': [
      for (var index = 0; index < 16; index++)
        {'session': index, 'samples': 1, 'p99_us': 1},
    ],
  },
  'active_output' => {
    'sessions': 16,
    'per_session': [
      for (var index = 0; index < 16; index++)
        {'session': index, 'elapsed_us': 1, 'mib_per_second': 1.0},
    ],
    'slowest_to_fastest_ratio': 1.0,
  },
  'spawn_close' => {'samples': 1},
  'observation' => {'repetitions': 1, 'mode_samples': 0},
  'forced_close' => {'descendant_reclaimed': true, 'exit_code': 1},
  'idle_1' => _idleResult(1),
  'idle_10' => _idleResult(10),
  'idle_100' => _idleResult(100),
  _ => const <String, Object?>{},
};

Map<String, Object?> _repeatedRun(String workload) => switch (workload) {
  'output' || 'transport_output' || 'input' || 'transport_input' => {
    'bytes': 1,
    'elapsed_us': 1,
    'mib_per_second': 1.0,
    'exit_code': 0,
  },
  'bidirectional' => {
    'sent_bytes': 1,
    'received_bytes': 0,
    'elapsed_us': 1,
    'aggregate_mib_per_second': 1.0,
    'exit_code': 0,
    'exact_output_history_supported': false,
    'integrity_scope': 'compact terminal report',
  },
  'pause_resume' => {
    'bytes': 1,
    'pause_ms': 1,
    'resume_to_eof_us': 1,
    'exit_code': 0,
  },
  'discard' => {'generated_bytes': 1, 'elapsed_us': 1, 'exit_code': 0},
  _ => const <String, Object?>{},
};

Map<String, Object?> _metadata({required int schema, required String suite}) {
  return {
    'schema': schema,
    'suite': suite,
    'revision': '0123456789abcdef',
    'tree_dirty': false,
    'platform': 'windows',
    'architecture': 'x64',
  };
}

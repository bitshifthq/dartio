import 'package:test/test.dart';

import '../../tool/validate_ci_result.dart';

void main() {
  group('validateCiResult', () {
    group('scorecard', () {
      test('accepts a complete scorecard artifact', () {
        final artifact = <String, Object?>{
          ..._metadata(schema: 4, suite: 'ptyx-diagnostic-scorecard'),
          for (final workload in scorecardWorkloads)
            workload: const <String, Object?>{},
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

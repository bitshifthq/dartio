import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:test/test.dart';

import '../../tool/verify_release_evidence.dart';

const _revision = '0123456789abcdef';
const _fixtureAotContent = 'qualification AOT fixture';
const _fixtureSourceContent = 'qualification fixture source';
const _fixtureDigest =
    '2222222222222222222222222222222222222222222222222222222222222222';
const _scorecardDigest =
    '3333333333333333333333333333333333333333333333333333333333333333';
const _oldWindowsVersion = 'Microsoft Windows [Version 10.0.19045.1]';
const _malformedWindowsVersion = 'Microsoft Windows [Version 10.0.26100.foo]';

Map<String, Object?> _acceptedManifest() => {
  'schema': 2,
  'acceptance_result': true,
  'revision': _revision,
  'tree_dirty': false,
  'runtime_targets': {
    'linux-x64': 'passed',
    'linux-arm64': 'passed',
    'macos-x64': 'passed',
    'macos-arm64': 'passed',
    'windows-x64': 'passed',
    'windows-arm64': 'passed',
  },
  'performance': {
    'direct_output_ratio': 0.90,
    'documented_platform_ceiling': false,
    'platform_ceiling_evidence': null,
  },
  'integrity': {
    'output_bytes': 2 * 1024 * 1024 * 1024,
    'input_bytes': 2 * 1024 * 1024 * 1024,
    'bidirectional_sent_bytes': 2 * 1024 * 1024 * 1024,
    'bidirectional_received_bytes': 2 * 1024 * 1024 * 1024,
  },
  'soak_duration_seconds': 48 * 60 * 60,
  'verification': {
    'address_sanitizer': 'passed',
    'leak_sanitizer': 'passed',
    'thread_sanitizer': 'passed',
    'fuzz': 'passed',
    'fault_injection': 'passed',
    'publication_dry_run': 'passed',
  },
  'evidence': {
    for (final key in const [
      'runtime-linux-x64',
      'runtime-linux-arm64',
      'runtime-macos-x64',
      'runtime-macos-arm64',
      'runtime-windows-x64',
      'runtime-windows-arm64',
      'performance',
      'integrity',
      'soak',
      'sanitizers',
      'fuzz',
      'fault-and-publication',
      'fixture-aot',
      'fixture-source',
    ])
      key: {
        'path': '$key.json',
        'sha256':
            '0000000000000000000000000000000000000000000000000000000000000000',
      },
  },
};

Map<String, Object?> _resultEvidence(String key) {
  final result = <String, Object?>{
    'schema': 1,
    'suite': 'ptyx-$key-acceptance',
    'revision': _revision,
    'tree_dirty': false,
    'passed': true,
    'platform': 'linux',
    'architecture': 'x64',
    'command': ['qualification', key],
    'checks': {'completed': true},
    'started_at_utc': '2026-01-01T00:00:00Z',
    'finished_at_utc': '2026-01-01T00:01:00Z',
  };
  if (key.startsWith('runtime-')) {
    final target = key.substring('runtime-'.length);
    final parts = target.split('-');
    result
      ..['target'] = target
      ..['platform'] = parts.first
      ..['architecture'] = parts.sublist(1).join('-')
      ..['platform_version'] = parts.first == 'windows'
          ? 'Microsoft Windows [Version 10.0.26100.1]'
          : 'test platform'
      ..['checks'] = {
        for (final name in const [
          'spawn',
          'output',
          'input',
          'bidirectional',
          'pause_resume',
          'input_saturation',
          'resize',
          'normal_exit',
          'forced_close',
          'descendant_cleanup',
          'resource_return',
        ])
          name: true,
      };
  } else if (key == 'performance') {
    result.addAll({
      'direct_output_ratio': 0.90,
      'production_output_mib_s': 90.0,
      'direct_output_mib_s': 100.0,
      'production_runs': [
        for (var index = 0; index < 3; index++)
          {
            'bytes': 128 * 1024 * 1024,
            'elapsed_us': 1_000_000,
            'mib_per_second': 90.0,
            'exit_code': 0,
          },
      ],
      'direct_runs': [
        for (var index = 0; index < 3; index++)
          {
            'bytes': 128 * 1024 * 1024,
            'elapsed_us': 900_000,
            'mib_per_second': 100.0,
            'exit_code': 0,
          },
      ],
      'repetitions': 3,
      'warmups': 1,
      'host': {
        'platform': 'linux',
        'architecture': 'x64',
        'dart_version': '3.11.0',
        'native_compiler': 'rustc 1.90.0',
      },
      'production_artifact_sha256':
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
      'direct_artifact_sha256':
          'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
    });
  } else if (key == 'sanitizers') {
    result['results'] = {
      for (final name in const [
        'address_sanitizer',
        'leak_sanitizer',
        'thread_sanitizer',
      ])
        name: {
          'exit_code': 0,
          'command': [
            'cargo',
            '+nightly-2026-07-20',
            'test',
            '-Zsanitizer=${name == 'thread_sanitizer' ? 'thread' : 'address'}',
          ],
        },
    };
  } else if (key == 'fuzz') {
    result['cargo_fuzz_version'] = 'cargo-fuzz 0.13.2';
    result['targets'] = {
      for (final name in const ['broker_decoder', 'controller_decoder'])
        name: {
          'exit_code': 0,
          'duration_seconds': 600,
          'command': [
            'cargo',
            '+nightly-2026-07-20',
            'fuzz',
            'run',
            name,
            '--',
            '-max_total_time=600',
            '-timeout=5',
          ],
          'started_at_utc': '2026-01-01T00:00:00Z',
          'finished_at_utc': '2026-01-01T00:10:00Z',
        },
    };
  } else if (key == 'fault-and-publication') {
    result.addAll({
      'fault_cases_passed': 6,
      'publication_dry_run': {
        'exit_code': 0,
        'command': ['dart', 'pub', 'publish', '--dry-run'],
      },
    });
  }
  return result;
}

Map<String, Object?> _soakEvidence() {
  const baseline = {
    'tree_rss_bytes': 1024,
    'tree_descriptors': 10,
    'tree_threads': 2,
    'tree_processes': 1,
  };
  return {
    'revision': _revision,
    'tree_dirty': false,
    'platform': 'linux',
    'architecture': 'x64',
    'schema': 1,
    'suite': 'ptyx-exact-integrity-soak',
    'command': ['48:00:00', '--fixture=/qualification/fixture'],
    'fixture_source_sha256': sha256
        .convert(utf8.encode(_fixtureSourceContent))
        .toString(),
    'fixture_executable': '/qualification/fixture',
    'fixture_executable_sha256': sha256
        .convert(utf8.encode(_fixtureAotContent))
        .toString(),
    'duration_seconds': 48 * 60 * 60,
    'started_at_utc': '2026-01-01T00:00:00Z',
    'finished_at_utc': '2026-01-03T00:00:00Z',
    'resource_before': baseline,
    'resource_samples': [
      for (var hour = 1; hour <= 48; hour++)
        {...baseline, 'elapsed_seconds': hour * 60 * 60},
    ],
    'resource_sampling_cycle_interval': 50,
    'resource_after': baseline,
    'resource_counts_stabilized': true,
    'peak_tree_rss_bytes': 1024,
    'rss_peak_kind': 'sampled-steady-state',
    'peak_rss_growth_bytes': 0,
    'steady_rss_growth_budget_bytes': 64 * 1024 * 1024,
    'steady_rss_within_growth_budget': true,
    'cleanup_rss_growth_budget_bytes': 32 * 1024 * 1024,
    'cleanup_rss_within_growth_budget': true,
    'cleanup_passed': true,
    'cycles': 1,
    'verified_bytes': 1,
    'long_lived_verified_bytes': 1,
  };
}

void main() {
  group('release evidence', () {
    test('accepts a complete exact-revision manifest', () {
      expect(verifyReleaseEvidence(_acceptedManifest(), _revision), isEmpty);
    });

    test('rejects missing runtime, stress, performance, and verification', () {
      final manifest = _acceptedManifest()
        ..['runtime_targets'] = <String, Object?>{}
        ..['performance'] = {
          'direct_output_ratio': 0.899,
          'documented_platform_ceiling': false,
        }
        ..['integrity'] = <String, Object?>{}
        ..['soak_duration_seconds'] = 47 * 60 * 60
        ..['verification'] = <String, Object?>{};

      final failures = verifyReleaseEvidence(manifest, _revision);

      expect(failures, hasLength(13));
      expect(failures, contains('all six runtime_targets must be passed'));
      expect(failures, contains('performance must clear 90% direct output'));
      expect(
        failures,
        contains('soak_duration_seconds must be at least 48 hours'),
      );
    });

    test('hashes every evidence file inside the bundle', () async {
      final directory = Directory.systemTemp.createTempSync(
        'ptyx-release-evidence-',
      );
      addTearDown(() => directory.deleteSync(recursive: true));
      final manifest = _acceptedManifest();
      final evidence = manifest['evidence']! as Map<String, Object?>;
      for (final entry in evidence.entries) {
        final descriptor = entry.value! as Map<String, Object?>;
        final content = switch (entry.key) {
          'soak' => jsonEncode(_soakEvidence()),
          'integrity' => jsonEncode({
            'schema': 4,
            'suite': 'ptyx-diagnostic-scorecard',
            'revision': _revision,
            'tree_dirty': false,
            'platform': 'linux',
            'architecture': 'x64',
            'command': ['integrity', '--integrity-bytes=2147483648'],
            'fixture_sha256': _fixtureDigest,
            'scorecard_sha256': _scorecardDigest,
            'integrity': {
              'output': {'bytes': 2 * 1024 * 1024 * 1024, 'exit_code': 0},
              'input': {'bytes': 2 * 1024 * 1024 * 1024, 'exit_code': 0},
              'bidirectional': {
                'sent_bytes': 2 * 1024 * 1024 * 1024,
                'received_bytes': 2 * 1024 * 1024 * 1024,
                'exit_code': 0,
              },
            },
          }),
          'fixture-aot' => _fixtureAotContent,
          'fixture-source' => _fixtureSourceContent,
          _ => jsonEncode(_resultEvidence(entry.key)),
        };
        final file = File('${directory.path}/${descriptor['path']}')
          ..writeAsStringSync(content);
        descriptor['sha256'] = sha256
            .convert(file.readAsBytesSync())
            .toString();
      }

      expect(await verifyEvidenceFiles(manifest, directory.path), isEmpty);

      File('${directory.path}/soak.json').writeAsStringSync('tampered');
      expect(
        await verifyEvidenceFiles(manifest, directory.path),
        contains('evidence.soak SHA-256 mismatch: soak.json'),
      );
    });

    test('rejects asserted results without semantic evidence', () async {
      final directory = Directory.systemTemp.createTempSync(
        'ptyx-release-evidence-invalid-',
      );
      addTearDown(() => directory.deleteSync(recursive: true));
      final manifest = _acceptedManifest();
      final evidence = manifest['evidence']! as Map<String, Object?>;
      for (final entry in evidence.entries) {
        final descriptor = entry.value! as Map<String, Object?>;
        final content = switch (entry.key) {
          'soak' => jsonEncode(
            _soakEvidence()
              ..['resource_after'] = {
                'tree_rss_bytes': 1024 * 1024,
                'tree_descriptors': 10,
                'tree_threads': 2,
                'tree_processes': 1,
              },
          ),
          'integrity' => jsonEncode({
            'schema': 4,
            'suite': 'ptyx-diagnostic-scorecard',
            'revision': _revision,
            'tree_dirty': false,
            'platform': 'linux',
            'architecture': 'x64',
            'command': ['integrity', '--integrity-bytes=2147483648'],
            'fixture_sha256': _fixtureDigest,
            'scorecard_sha256': _scorecardDigest,
            'integrity': {
              'output': {'bytes': 2 * 1024 * 1024 * 1024, 'exit_code': 0},
              'input': {'bytes': 2 * 1024 * 1024 * 1024, 'exit_code': 0},
              'bidirectional': {
                'sent_bytes': 2 * 1024 * 1024 * 1024,
                'received_bytes': 2 * 1024 * 1024 * 1024,
                'exit_code': 0,
              },
            },
          }),
          'fixture-aot' => _fixtureAotContent,
          'fixture-source' => _fixtureSourceContent,
          'fuzz' => jsonEncode({
            ..._resultEvidence('fuzz'),
            'targets': {
              for (final name in const ['broker_decoder', 'controller_decoder'])
                name: {
                  'exit_code': 0,
                  'duration_seconds': 600,
                  'command': ['sleep', '600'],
                  'started_at_utc': '2026-01-01T00:00:00Z',
                  'finished_at_utc': '2026-01-01T00:10:00Z',
                },
            },
          }),
          'performance' => jsonEncode({
            ..._resultEvidence('performance'),
            'production_runs': <Object?>[],
          }),
          _ => jsonEncode(switch (entry.key) {
            'runtime-windows-x64' => {
              ..._resultEvidence(entry.key),
              'platform_version': _oldWindowsVersion,
            },
            'runtime-windows-arm64' => {
              ..._resultEvidence(entry.key),
              'platform_version': _malformedWindowsVersion,
            },
            _ => _resultEvidence(entry.key),
          }),
        };
        final file = File('${directory.path}/${descriptor['path']}')
          ..writeAsStringSync(content);
        descriptor['sha256'] = sha256
            .convert(file.readAsBytesSync())
            .toString();
      }

      expect(
        await verifyEvidenceFiles(manifest, directory.path),
        allOf(
          contains('soak evidence must pass resource and RSS gates'),
          contains('fuzz evidence must prove both decoder targets'),
          contains(
            'performance evidence must include reproducible raw runs, '
            'provenance, and an independently recomputed gate',
          ),
          contains(
            'runtime-windows-x64 evidence must run on Windows build '
            '26100 or newer',
          ),
          contains(
            'runtime-windows-arm64 evidence must run on Windows build '
            '26100 or newer',
          ),
        ),
      );
    });
  });
}

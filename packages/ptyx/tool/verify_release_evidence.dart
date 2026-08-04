import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';

const _targets = {
  'linux-x64',
  'linux-arm64',
  'macos-x64',
  'macos-arm64',
  'windows-x64',
  'windows-arm64',
};
const _multiGigabyte = 2 * 1024 * 1024 * 1024;
const _multiDaySeconds = 48 * 60 * 60;
const _minimumWindowsBuild = 26100;
const _minimumBenchmarkBytes = 128 * 1024 * 1024;
const _productionSteadyRssBudget = 64 * 1024 * 1024;
const _productionCleanupRssBudget = 32 * 1024 * 1024;
const _evidenceKeys = {
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
  'publication',
  'fixture-aot',
  'fixture-source',
};

Future<void> main(List<String> arguments) async {
  final path = arguments
      .where((argument) => !argument.startsWith('--'))
      .singleOrNull;
  final revision = arguments
      .where((argument) => argument.startsWith('--revision='))
      .map((argument) => argument.substring('--revision='.length))
      .singleOrNull;
  final evidenceRoot = arguments
      .where((argument) => argument.startsWith('--evidence-root='))
      .map((argument) => argument.substring('--evidence-root='.length))
      .singleOrNull;
  if (path == null || revision == null || evidenceRoot == null) {
    stderr.writeln(
      'usage: dart run tool/verify_release_evidence.dart '
      '<manifest.json> --revision=<git-sha> --evidence-root=<directory>',
    );
    exitCode = 64;
    return;
  }

  final manifest = jsonDecode(File(path).readAsStringSync());
  if (manifest is! Map<String, Object?>) {
    throw const FormatException('release evidence must be a JSON object');
  }
  final failures = [
    ...verifyReleaseEvidence(manifest, revision),
    ...await verifyEvidenceFiles(manifest, evidenceRoot),
  ];
  if (failures.isNotEmpty) {
    for (final failure in failures) {
      stderr.writeln('release evidence: $failure');
    }
    exitCode = 1;
    return;
  }
  stdout.writeln('ptyx release evidence accepted for $revision');
}

List<String> verifyReleaseEvidence(
  Map<String, Object?> manifest,
  String revision,
) {
  final failures = <String>[];
  void require(String message, {required bool condition}) {
    if (!condition) failures.add(message);
  }

  require(
    'acceptance_result must be true',
    condition: manifest['acceptance_result'] == true,
  );
  require('schema must be 2', condition: manifest['schema'] == 2);
  require(
    'revision must match the release',
    condition: manifest['revision'] == revision,
  );
  require(
    'tree_dirty must be false',
    condition: manifest['tree_dirty'] == false,
  );

  final targets = manifest['runtime_targets'];
  require(
    'all six runtime_targets must be passed',
    condition:
        targets is Map<String, Object?> &&
        _targets.every((target) => targets[target] == 'passed'),
  );

  final performance = manifest['performance'];
  final directRatio = performance is Map<String, Object?>
      ? performance['direct_output_ratio']
      : null;
  require(
    'performance must clear 90% direct output',
    condition: directRatio is num && directRatio >= 0.90,
  );

  final integrity = manifest['integrity'];
  for (final field in const [
    'output_bytes',
    'input_bytes',
    'bidirectional_sent_bytes',
    'bidirectional_received_bytes',
  ]) {
    require(
      'integrity.$field must be at least 2 GiB',
      condition:
          integrity is Map<String, Object?> &&
          integrity[field] is int &&
          (integrity[field]! as int) >= _multiGigabyte,
    );
  }
  require(
    'soak_duration_seconds must be at least 48 hours',
    condition:
        manifest['soak_duration_seconds'] is int &&
        (manifest['soak_duration_seconds']! as int) >= _multiDaySeconds,
  );

  final verification = manifest['verification'];
  for (final field in const [
    'address_sanitizer',
    'leak_sanitizer',
    'thread_sanitizer',
    'fuzz',
    'publication_dry_run',
  ]) {
    require(
      'verification.$field must be passed',
      condition:
          verification is Map<String, Object?> &&
          verification[field] == 'passed',
    );
  }

  final evidence = manifest['evidence'];
  for (final key in _evidenceKeys) {
    final descriptor = evidence is Map<String, Object?> ? evidence[key] : null;
    require(
      'evidence.$key must contain a relative path and SHA-256 digest',
      condition:
          descriptor is Map<String, Object?> &&
          descriptor['path'] is String &&
          (descriptor['path']! as String).isNotEmpty &&
          descriptor['sha256'] is String &&
          RegExp(r'^[a-f0-9]{64}$').hasMatch(descriptor['sha256']! as String),
    );
  }
  return failures;
}

Future<List<String>> verifyEvidenceFiles(
  Map<String, Object?> manifest,
  String evidenceRoot,
) async {
  final failures = <String>[];
  final rootDirectory = Directory(evidenceRoot).absolute;
  final root = rootDirectory.resolveSymbolicLinksSync();
  final evidence = manifest['evidence'];
  if (evidence is! Map<String, Object?>) {
    return const ['evidence must be a JSON object'];
  }
  for (final key in _evidenceKeys) {
    final descriptor = evidence[key];
    if (descriptor is! Map<String, Object?> ||
        descriptor['path'] is! String ||
        descriptor['sha256'] is! String) {
      continue;
    }
    final relativePath = descriptor['path']! as String;
    final file = File('$root${Platform.pathSeparator}$relativePath').absolute;
    if (relativePath.isEmpty ||
        RegExp(r'^(?:[/\\]|[A-Za-z]:[/\\])').hasMatch(relativePath) ||
        relativePath.split(RegExp(r'[/\\]')).contains('..') ||
        !file.path.startsWith('$root${Platform.pathSeparator}')) {
      failures.add('evidence.$key path must remain inside the evidence bundle');
      continue;
    }
    if (!file.existsSync()) {
      failures.add('evidence.$key file is missing: $relativePath');
      continue;
    }
    if (FileSystemEntity.typeSync(file.path, followLinks: false) !=
        FileSystemEntityType.file) {
      failures.add('evidence.$key must be a regular file: $relativePath');
      continue;
    }
    final resolved = file.resolveSymbolicLinksSync();
    if (!resolved.startsWith('$root${Platform.pathSeparator}')) {
      failures.add('evidence.$key path must remain inside the evidence bundle');
      continue;
    }
    final actual = sha256.convert(file.readAsBytesSync()).toString();
    if (actual != descriptor['sha256']) {
      failures.add('evidence.$key SHA-256 mismatch: $relativePath');
      continue;
    }
    if (key == 'soak') {
      failures.addAll(_verifySoak(file, manifest));
    } else if (key == 'integrity') {
      failures.addAll(_verifyIntegrity(file, manifest));
    } else if (key != 'fixture-aot' && key != 'fixture-source') {
      failures.addAll(_verifyResult(key, file, manifest));
    }
  }
  return failures;
}

List<String> _verifyResult(
  String key,
  File file,
  Map<String, Object?> manifest,
) {
  final failures = <String>[];
  final artifact = _jsonObject(file, failures, key);
  if (artifact == null) return failures;
  final command = artifact['command'];
  final checks = artifact['checks'];
  final started = DateTime.tryParse('${artifact['started_at_utc']}');
  final finished = DateTime.tryParse('${artifact['finished_at_utc']}');
  if (artifact['schema'] != 1 ||
      artifact['suite'] != 'ptyx-$key-acceptance' ||
      artifact['revision'] != manifest['revision'] ||
      artifact['tree_dirty'] != false ||
      artifact['passed'] != true ||
      artifact['platform'] is! String ||
      (artifact['platform']! as String).isEmpty ||
      artifact['architecture'] is! String ||
      (artifact['architecture']! as String).isEmpty ||
      command is! List<Object?> ||
      command.isEmpty ||
      !command.every((argument) => argument is String && argument.isNotEmpty) ||
      checks is! Map<String, Object?> ||
      checks.isEmpty ||
      !checks.values.every((value) => value == true) ||
      started == null ||
      finished == null ||
      finished.isBefore(started)) {
    failures.add(
      '$key evidence must contain passing exact-revision provenance',
    );
    return failures;
  }
  if (key.startsWith('runtime-')) {
    final target = key.substring('runtime-'.length);
    final targetParts = target.split('-');
    if (artifact['target'] != target ||
        artifact['platform'] != targetParts.first ||
        artifact['architecture'] != targetParts.sublist(1).join('-') ||
        !const {
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
        }.every((name) => checks[name] == true)) {
      failures.add('$key evidence must prove the required runtime checks');
    }
    if (target.startsWith('windows-') &&
        !_windowsBuildAtLeast(
          artifact['platform_version'],
          _minimumWindowsBuild,
        )) {
      failures.add(
        '$key evidence must run on Windows build '
        '$_minimumWindowsBuild or newer',
      );
    }
  } else if (key == 'performance') {
    final performance = manifest['performance'];
    final ratio = artifact['direct_output_ratio'];
    final production = artifact['production_output_mib_s'];
    final direct = artifact['direct_output_mib_s'];
    final productionRuns = artifact['production_runs'];
    final directRuns = artifact['direct_runs'];
    final repetitions = artifact['repetitions'];
    final warmups = artifact['warmups'];
    final host = artifact['host'];
    final productionDigest = artifact['production_artifact_sha256'];
    final directDigest = artifact['direct_artifact_sha256'];
    final measuredProduction = _medianThroughput(productionRuns);
    final measuredDirect = _medianThroughput(directRuns);
    final measuredRatio =
        measuredProduction != null &&
            measuredDirect != null &&
            measuredDirect > 0
        ? measuredProduction / measuredDirect
        : null;
    final hasProvenance =
        host is Map<String, Object?> &&
        host['platform'] is String &&
        (host['platform']! as String).isNotEmpty &&
        host['architecture'] is String &&
        (host['architecture']! as String).isNotEmpty &&
        host['dart_version'] is String &&
        (host['dart_version']! as String).isNotEmpty &&
        host['native_compiler'] is String &&
        (host['native_compiler']! as String).isNotEmpty &&
        productionDigest is String &&
        RegExp(r'^[a-f0-9]{64}$').hasMatch(productionDigest) &&
        directDigest is String &&
        RegExp(r'^[a-f0-9]{64}$').hasMatch(directDigest);
    if (performance is! Map<String, Object?> ||
        ratio != performance['direct_output_ratio'] ||
        ratio is! num ||
        ratio < 0.90 ||
        repetitions is! int ||
        repetitions < 3 ||
        warmups is! int ||
        warmups < 1 ||
        !_validThroughputRuns(productionRuns, repetitions) ||
        !_validThroughputRuns(directRuns, repetitions) ||
        !hasProvenance ||
        production is! num ||
        direct is! num ||
        measuredProduction == null ||
        measuredDirect == null ||
        (production - measuredProduction).abs() > 0.000001 ||
        (direct - measuredDirect).abs() > 0.000001 ||
        measuredRatio == null ||
        measuredRatio + 0.000001 < 0.90 ||
        (measuredRatio - ratio).abs() > 0.000001) {
      failures.add(
        'performance evidence must include reproducible raw runs, '
        'provenance, and an independently recomputed gate',
      );
    }
  } else if (key == 'sanitizers') {
    final results = artifact['results'];
    if (results is! Map<String, Object?> ||
        !const [
          'address_sanitizer',
          'leak_sanitizer',
          'thread_sanitizer',
        ].every((name) {
          final result = results[name];
          return result is Map<String, Object?> &&
              result['exit_code'] == 0 &&
              result['command'] is List<Object?> &&
              _matchesSanitizerCommand(
                result['command']! as List<Object?>,
                name,
              ) &&
              _matchesSanitizerEnvironment(result, name);
        })) {
      failures.add('sanitizer evidence must prove ASan, LSan, and TSan');
    }
  } else if (key == 'fuzz') {
    final targets = artifact['targets'];
    if (artifact['cargo_fuzz_version'] != 'cargo-fuzz 0.13.2' ||
        targets is! Map<String, Object?> ||
        !const ['broker_decoder', 'controller_decoder'].every((name) {
          final result = targets[name];
          final started = result is Map<String, Object?>
              ? DateTime.tryParse('${result['started_at_utc']}')
              : null;
          final finished = result is Map<String, Object?>
              ? DateTime.tryParse('${result['finished_at_utc']}')
              : null;
          final duration = result is Map<String, Object?>
              ? result['duration_seconds']
              : null;
          final command = result is Map<String, Object?>
              ? result['command']
              : null;
          return result is Map<String, Object?> &&
              result['exit_code'] == 0 &&
              duration is int &&
              duration >= 600 &&
              command is List<Object?> &&
              _matchesFuzzCommand(command, name) &&
              started != null &&
              finished != null &&
              !finished.isBefore(started) &&
              finished.difference(started).inSeconds >= duration;
        })) {
      failures.add('fuzz evidence must prove both decoder targets');
    }
  } else if (key == 'publication') {
    final dryRun = artifact['publication_dry_run'];
    if (dryRun is! Map<String, Object?> ||
        dryRun['exit_code'] != 0 ||
        dryRun['command'] is! List<Object?> ||
        (dryRun['command']! as List<Object?>).isEmpty) {
      failures.add('publication evidence must prove a successful dry run');
    }
  }
  return failures;
}

bool _windowsBuildAtLeast(Object? platformVersion, int minimumBuild) {
  if (platformVersion is! String) return false;
  final match = RegExp(
    r'\b10\.0\.(\d+)(?:\.\d+)?(?![\d.])\b',
  ).firstMatch(platformVersion);
  final build = int.tryParse(match?.group(1) ?? '');
  return build != null && build >= minimumBuild;
}

bool _matchesFuzzCommand(List<Object?> command, String target) {
  final expected = <Object?>[
    'cargo',
    '+nightly-2026-07-20',
    'fuzz',
    'run',
    target,
    '--',
    '-max_total_time=600',
    '-timeout=5',
  ];
  if (command.length != expected.length) return false;
  for (var index = 0; index < expected.length; index++) {
    if (command[index] != expected[index]) return false;
  }
  return true;
}

bool _validThroughputRuns(Object? value, int repetitions) {
  if (value is! List<Object?> || value.length != repetitions) return false;
  final bytes = <int>{};
  for (final run in value) {
    if (run is! Map<String, Object?> ||
        run['bytes'] is! int ||
        (run['bytes']! as int) < _minimumBenchmarkBytes ||
        run['elapsed_us'] is! int ||
        (run['elapsed_us']! as int) <= 0 ||
        run['mib_per_second'] is! num ||
        (run['mib_per_second']! as num) <= 0 ||
        run['exit_code'] != 0) {
      return false;
    }
    final measured =
        (run['bytes']! as int) /
        (1024 * 1024) /
        ((run['elapsed_us']! as int) / 1000000);
    final supplied = (run['mib_per_second']! as num).toDouble();
    if ((supplied - measured).abs() > 0.000001) return false;
    bytes.add(run['bytes']! as int);
  }
  return bytes.length == 1;
}

double? _medianThroughput(Object? value) {
  if (value is! List<Object?> || value.isEmpty) return null;
  final runs = [
    for (final run in value)
      if (run is Map<String, Object?> && run['mib_per_second'] is num)
        (run['mib_per_second']! as num).toDouble(),
  ];
  if (runs.length != value.length) return null;
  runs.sort();
  final middle = runs.length ~/ 2;
  return runs.length.isOdd
      ? runs[middle]
      : (runs[middle - 1] + runs[middle]) / 2;
}

bool _matchesSanitizerCommand(List<Object?> command, String name) {
  final sanitizer = switch (name) {
    'address_sanitizer' || 'leak_sanitizer' => 'address',
    'thread_sanitizer' => 'thread',
    _ => null,
  };
  return sanitizer != null &&
      command.contains('cargo') &&
      command.contains('test') &&
      command.any(
        (argument) =>
            argument is String && argument.contains('-Zsanitizer=$sanitizer'),
      );
}

bool _matchesSanitizerEnvironment(Map<String, Object?> result, String name) {
  final environment = result['environment'];
  if (environment is! Map<String, Object?>) return false;
  final variable = name == 'thread_sanitizer' ? 'TSAN_OPTIONS' : 'ASAN_OPTIONS';
  final options = environment[variable];
  if (options is! String || !options.contains('halt_on_error=1')) return false;
  return name != 'leak_sanitizer' || options.contains('detect_leaks=1');
}

List<String> _verifySoak(File file, Map<String, Object?> manifest) {
  final failures = <String>[];
  final artifact = _jsonObject(file, failures, 'soak');
  if (artifact == null) return failures;
  final command = artifact['command'];
  if (artifact['schema'] != 1 ||
      artifact['suite'] != 'ptyx-exact-integrity-soak' ||
      artifact['revision'] != manifest['revision'] ||
      artifact['tree_dirty'] != false ||
      artifact['platform'] is! String ||
      !const {'linux', 'macos', 'windows'}.contains(artifact['platform']) ||
      artifact['architecture'] is! String ||
      (artifact['architecture']! as String).isEmpty ||
      command is! List<Object?> ||
      command.isEmpty) {
    failures.add('soak evidence must have exact-revision tool provenance');
  }
  final duration = artifact['duration_seconds'];
  final started = DateTime.tryParse('${artifact['started_at_utc']}');
  final finished = DateTime.tryParse('${artifact['finished_at_utc']}');
  if (duration is! int ||
      duration < _multiDaySeconds ||
      started == null ||
      finished == null ||
      finished.difference(started).inSeconds < duration) {
    failures.add('soak evidence must prove at least 48 elapsed hours');
  }
  final before = artifact['resource_before'];
  final after = artifact['resource_after'];
  final samples = artifact['resource_samples'];
  final platform = artifact['platform'];
  final resourceKey = platform == 'windows'
      ? 'tree_handles'
      : 'tree_descriptors';
  final cleanupBudget = artifact['cleanup_rss_growth_budget_bytes'];
  final steadyBudget = artifact['steady_rss_growth_budget_bytes'];
  final fixtureExecutable = artifact['fixture_executable'];
  final fixtureExecutableHash = artifact['fixture_executable_sha256'];
  final fixtureSourceHash = artifact['fixture_source_sha256'];
  final evidence = manifest['evidence'];
  final aotDescriptor = evidence is Map<String, Object?>
      ? evidence['fixture-aot']
      : null;
  final sourceDescriptor = evidence is Map<String, Object?>
      ? evidence['fixture-source']
      : null;
  final snapshots = [
    if (before is Map<String, Object?>) before,
    if (samples is List<Object?>) ...samples.whereType<Map<String, Object?>>(),
    if (after is Map<String, Object?>) after,
  ];
  final structurallyValid =
      before is Map<String, Object?> &&
      after is Map<String, Object?> &&
      samples is List<Object?> &&
      samples.length >= 48 &&
      artifact['resource_sampling_cycle_interval'] is int &&
      (artifact['resource_sampling_cycle_interval']! as int) > 0 &&
      artifact['rss_peak_kind'] == 'sampled-steady-state' &&
      fixtureExecutable is String &&
      fixtureExecutable.isNotEmpty &&
      fixtureExecutableHash is String &&
      RegExp(r'^[a-f0-9]{64}$').hasMatch(fixtureExecutableHash) &&
      fixtureSourceHash is String &&
      RegExp(r'^[a-f0-9]{64}$').hasMatch(fixtureSourceHash) &&
      aotDescriptor is Map<String, Object?> &&
      fixtureExecutableHash == aotDescriptor['sha256'] &&
      sourceDescriptor is Map<String, Object?> &&
      fixtureSourceHash == sourceDescriptor['sha256'] &&
      snapshots.every(
        (snapshot) =>
            snapshot['tree_rss_bytes'] is int &&
            snapshot[resourceKey] is int &&
            snapshot['tree_threads'] is int &&
            snapshot['tree_processes'] is int,
      ) &&
      cleanupBudget == _productionCleanupRssBudget &&
      steadyBudget == _productionSteadyRssBudget;
  var resourceGatePassed = false;
  if (structurallyValid) {
    final resourceBefore = before;
    final resourceAfter = after;
    final resourceSamples = samples;
    const cleanupGrowthBudget = _productionCleanupRssBudget;
    const steadyGrowthBudget = _productionSteadyRssBudget;
    final baselineRss = resourceBefore['tree_rss_bytes']! as int;
    final finalRss = resourceAfter['tree_rss_bytes']! as int;
    final peakRss = snapshots
        .map((snapshot) => snapshot['tree_rss_bytes']! as int)
        .reduce((left, right) => left > right ? left : right);
    final elapsedSamples = resourceSamples
        .whereType<Map<String, Object?>>()
        .map((sample) => sample['elapsed_seconds'])
        .whereType<int>()
        .toList();
    final samplesCoverRun =
        elapsedSamples.length == resourceSamples.length &&
        elapsedSamples.indexed.every(
          (entry) => entry.$1 == 0 || entry.$2 > elapsedSamples[entry.$1 - 1],
        ) &&
        elapsedSamples.last >= _multiDaySeconds - 60 * 60;
    final countsStable =
        (resourceAfter[resourceKey]! as int) <=
            (resourceBefore[resourceKey]! as int) &&
        (resourceAfter['tree_threads']! as int) <=
            (resourceBefore['tree_threads']! as int) &&
        (resourceAfter['tree_processes']! as int) <=
            (resourceBefore['tree_processes']! as int);
    final cleanupWithinBudget = finalRss <= baselineRss + cleanupGrowthBudget;
    final steadyWithinBudget = peakRss <= baselineRss + steadyGrowthBudget;
    resourceGatePassed =
        samplesCoverRun &&
        countsStable &&
        cleanupWithinBudget &&
        steadyWithinBudget &&
        artifact['peak_tree_rss_bytes'] == peakRss &&
        artifact['peak_rss_growth_bytes'] == peakRss - baselineRss &&
        artifact['resource_counts_stabilized'] == countsStable &&
        artifact['cleanup_rss_within_growth_budget'] == cleanupWithinBudget &&
        artifact['steady_rss_within_growth_budget'] == steadyWithinBudget &&
        artifact['cleanup_passed'] == true;
  }
  if (!resourceGatePassed) {
    failures.add('soak evidence must pass resource and RSS gates');
  }
  if (artifact['cycles'] is! int ||
      (artifact['cycles']! as int) <= 0 ||
      artifact['verified_bytes'] is! int ||
      (artifact['verified_bytes']! as int) <= 0 ||
      artifact['long_lived_verified_bytes'] is! int ||
      (artifact['long_lived_verified_bytes']! as int) <= 0) {
    failures.add('soak evidence must contain verified lifecycle traffic');
  }
  return failures;
}

List<String> _verifyIntegrity(File file, Map<String, Object?> manifest) {
  final failures = <String>[];
  final artifact = _jsonObject(file, failures, 'integrity');
  if (artifact == null) return failures;
  final command = artifact['command'];
  if (artifact['schema'] != 4 ||
      artifact['suite'] != 'ptyx-diagnostic-scorecard' ||
      artifact['revision'] != manifest['revision'] ||
      artifact['tree_dirty'] != false ||
      artifact['platform'] is! String ||
      !const {'linux', 'macos', 'windows'}.contains(artifact['platform']) ||
      artifact['architecture'] is! String ||
      (artifact['architecture']! as String).isEmpty ||
      command is! List<Object?> ||
      !command.contains('integrity') ||
      artifact['fixture_sha256'] is! String ||
      !RegExp(
        r'^[a-f0-9]{64}$',
      ).hasMatch(artifact['fixture_sha256']! as String) ||
      artifact['scorecard_sha256'] is! String ||
      !RegExp(
        r'^[a-f0-9]{64}$',
      ).hasMatch(artifact['scorecard_sha256']! as String)) {
    failures.add('integrity evidence must have exact-revision tool provenance');
  }
  final integrity = artifact['integrity'];
  final output = integrity is Map<String, Object?> ? integrity['output'] : null;
  final input = integrity is Map<String, Object?> ? integrity['input'] : null;
  final bidirectional = integrity is Map<String, Object?>
      ? integrity['bidirectional']
      : null;
  final valid =
      output is Map<String, Object?> &&
      output['bytes'] is int &&
      (output['bytes']! as int) >= _multiGigabyte &&
      output['exit_code'] == 0 &&
      input is Map<String, Object?> &&
      input['bytes'] is int &&
      (input['bytes']! as int) >= _multiGigabyte &&
      input['exit_code'] == 0 &&
      bidirectional is Map<String, Object?> &&
      bidirectional['sent_bytes'] is int &&
      (bidirectional['sent_bytes']! as int) >= _multiGigabyte &&
      bidirectional['received_bytes'] is int &&
      (bidirectional['received_bytes']! as int) >= _multiGigabyte &&
      bidirectional['exit_code'] == 0;
  if (!valid) {
    failures.add('integrity evidence must prove four exact 2 GiB directions');
  }
  return failures;
}

Map<String, Object?>? _jsonObject(
  File file,
  List<String> failures,
  String label,
) {
  try {
    final value = jsonDecode(file.readAsStringSync());
    if (value is Map<String, Object?>) return value;
  } on FormatException {
    // Report one stable validation failure below.
  }
  failures.add('$label evidence must be a JSON object');
  return null;
}

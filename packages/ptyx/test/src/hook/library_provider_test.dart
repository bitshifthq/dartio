import 'package:code_assets/code_assets.dart' show Architecture, OS;
import 'package:hooks/hooks.dart' show BuildInput;
import 'package:ptyx/src/hook/library_provider.dart'
    show
        AutoProvider,
        CompileFromSource,
        DownloadPrebuilt,
        LibraryProvider,
        cargoBuildDirectory,
        libraryExtension;
import 'package:test/test.dart';

void main() {
  group('LibraryProvider', () {
    BuildInput createBuildInput({
      OS os = .macOS,
      Architecture architecture = .arm64,
      Map<String, String> userDefines = const {},
    }) {
      return BuildInput(<String, Object?>{
        'package_name': 'ptyx',
        'package_root': '.',
        'out_dir': 'out',
        'out_dir_shared': 'shared',
        'user_defines': <String, Object?>{
          'workspace_pubspec': <String, Object?>{
            'base_path': '.',
            'defines': <String, Object?>{...userDefines},
          },
        },
        'config': <String, Object?>{
          'build_code_assets': true,
          'build_asset_types': <String>[],
          'extensions': <String, Object?>{
            'code_assets': <String, Object?>{
              'target_os': os.name,
              'target_architecture': architecture.name,
            },
          },
        },
      });
    }

    group('resolve', () {
      test('returns the requested provider', () {
        final providers = [
          LibraryProvider.resolve(createBuildInput()),
          LibraryProvider.resolve(
            createBuildInput(userDefines: {'source': 'compile'}),
          ),
          LibraryProvider.resolve(
            createBuildInput(userDefines: {'source': 'prebuilt'}),
          ),
        ];
        final providerTypes = (
          auto: providers[0] is AutoProvider,
          compile: providers[1] is CompileFromSource,
          prebuilt: providers[2] is DownloadPrebuilt,
        );

        expect(providerTypes, (auto: true, compile: true, prebuilt: true));
      });

      test('throws ArgumentError for unknown source values', () {
        final input = createBuildInput(userDefines: {'source': 'magic'});

        expect(
          () => LibraryProvider.resolve(input),
          throwsA(isA<ArgumentError>()),
        );
      });
    });

    group('libraryExtension', () {
      test('maps dynamic library extensions by operating system', () {
        final extensions = [
          libraryExtension(.macOS),
          libraryExtension(.linux),
          libraryExtension(.android),
          libraryExtension(.windows),
        ];

        expect(extensions, ['dylib', 'so', 'so', 'dll']);
      });
    });

    group('cargoBuildDirectory', () {
      test('places Cargo intermediates under the shared output directory', () {
        final directory = cargoBuildDirectory(createBuildInput());

        expect(
          directory.uri.pathSegments,
          containsAllInOrder(['shared', 'cargo', 'aarch64-macos']),
        );
      });
    });
  });
}

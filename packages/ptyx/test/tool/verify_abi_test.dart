import 'package:test/test.dart';

import '../../tool/verify_abi.dart';

void main() {
  test('export parser ignores a Windows library heading', () {
    const output = r'''
Dump of file C:\work\ptyx_c.dll

  ordinal hint RVA      name
        1    0 00001230 ptyx_abi_version
        2    1 00004560 ptyx_session_write
''';

    expect(parseExportedPtySymbols(output), {
      'ptyx_abi_version',
      'ptyx_session_write',
    });
  });

  test('export parser retains a real symbol matching the library stem', () {
    const output = r'''
Dump of file C:\work\ptyx_c.dll

  ordinal hint RVA      name
        1    0 00001230 ptyx_c
''';

    expect(parseExportedPtySymbols(output), {'ptyx_c'});
  });

  test('export parser accepts llvm-readobj and nm output', () {
    const output = '''
  Name: ptyx_runtime_create
0000000000001230 T _ptyx_session_close
''';

    expect(parseExportedPtySymbols(output), {
      'ptyx_runtime_create',
      'ptyx_session_close',
    });
  });
}

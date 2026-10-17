import re, sys
base = (sys.argv[1] if len(sys.argv) > 1 else '../autopsy') + '/Core/src/org/sleuthkit/autopsy/report/infrastructure/'
src = open(base + 'TableReportGenerator.java').read().split('\n')
bundle = {}
for line in open(base + 'Bundle.properties-MERGED', encoding='latin-1'):
    if '=' in line and not line.startswith('#'):
        k, v = line.rstrip('\n').split('=', 1)
        bundle[k.strip()] = v.strip()
body = '\n'.join(src[1283:1835])
start = body.index('if (BlackboardArtifact.ARTIFACT_TYPE.TSK_WEB_BOOKMARK')
end = body.index('} else {\n            // This is the case that it is a custom type')
chain = body[start:end]
# split into blocks at "} else if (" boundaries
parts = re.split(r'\}\s*else if\s*\(', chain)
parts[0] = parts[0][len('if ('):]
blocks = []
for p in parts:
    cond, _, rest = p.partition(') {')
    types = re.findall(r'ARTIFACT_TYPE\.(TSK_[A-Z_]+)', cond)
    cols = []
    for m in re.finditer(r'new (AttributeColumn|HeaderOnlyColumn|SourceFileColumn|StatusColumn)\((?:NbBundle\.getMessage\(this\.getClass\(\), "([^"]+)"\))?(?:,\s*new BlackboardAttribute\.Type\(BlackboardAttribute\.ATTRIBUTE_TYPE\.(TSK_[A-Z_]+)\))?', rest, re.S):
        kind, key, attr = m.groups()
        header = bundle.get(key, key) if key else 'Status'
        cols.append((kind, header, attr))
    removes = re.findall(r'attributeTypeSet\.remove\(new Type\(BlackboardAttribute\.ATTRIBUTE_TYPE\.(TSK_[A-Z_]+)\)\)', rest)
    blocks.append((types, cols, removes))
tail = body[body.index('// Source file column is added here for ordering purposes.'):]
tail = tail[:tail.index('columns.add(new SourceFileColumn')]
src_types = re.findall(r'ARTIFACT_TYPE\.(TSK_[A-Z_]+)', tail)
srcfile = bundle['ReportGenerator.artTableColHdr.srcFile']
tags = bundle['ReportGenerator.artTableColHdr.tags']
out = []
out.append('//! Autopsy\'s report table columns per artifact type, generated from')
out.append('//! `TableReportGenerator.getArtifactTableColumns` and its bundle by')
out.append('//! `scripts/gen/report_columns.py`. Do not edit by hand.')
out.append('')
out.append('use crate::table::Col;')
out.append('')
out.append('/// `(artifact types, leading columns, attribute types left out)`.')
out.append('pub(crate) const TYPE_COLUMNS: &[(&[&str], &[Col], &[&str])] = &[')
for types, cols, removes in blocks:
    cs = []
    for kind, header, attr in cols:
        h = header.replace('\\', '\\\\').replace('"', '\\"')
        if kind == 'AttributeColumn':
            cs.append(f'Col::Attr("{h}", "{attr}")')
        elif kind == 'HeaderOnlyColumn':
            cs.append(f'Col::Special("{h}")')
        elif kind == 'SourceFileColumn':
            cs.append(f'Col::SourceFile("{h}")')
        else:
            cs.append('Col::Status')
    out.append('    (')
    out.append('        &[' + ', '.join(f'"{t}"' for t in types) + '],')
    out.append('        &[' + ', '.join(cs) + '],')
    out.append('        &[' + ', '.join(f'"{r}"' for r in removes) + '],')
    out.append('    ),')
out.append('];')
out.append('')
out.append('/// Types whose table ends with the source file column (before Tags).')
out.append('pub(crate) const SOURCE_FILE_LAST: &[&str] = &[' + ', '.join(f'"{t}"' for t in src_types) + '];')
out.append('')
out.append(f'pub(crate) const SOURCE_FILE: &str = "{srcfile}";')
out.append(f'pub(crate) const TAGS: &str = "{tags}";')
print('\n'.join(out))
print(len(blocks), file=sys.stderr)

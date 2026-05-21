import type { CodegenConfig } from '@graphql-codegen/cli';

const config: CodegenConfig = {
    schema: 'http://localhost:3000/v1/library',
    documents: ['src/**/*.tsx'],
    ignoreNoDocuments: true, // for better experience with the watcher
    generates: {
        './src/gql/': {
            preset: 'client',
            plugins: [],
        },
        './src/schema.ts': {
            plugins: ['urql-introspection'],
            config: {
                useTypeImports: true,
                includeScalars: true,
                includeEnums: true,
                includeInputs: true,
                includeDirectives: true,
            },
        },
    },
};

export default config;

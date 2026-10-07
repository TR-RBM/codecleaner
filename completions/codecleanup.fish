# fish completion for codecleanup
#
# Install it with: codecleanup --completions=fish > ~/.config/fish/completions/codecleanup.fish

complete -c codecleanup -f
complete -c codecleanup -l in -r -F -d 'Source file, or folder to process recursively'
complete -c codecleanup -l docs -r -F -d 'Docs folder or .md file that receives the comments'
complete -c codecleanup -l restore -d 'Replace refids with the comments stored in the docs'
complete -c codecleanup -l plugins -r -f -a '(__fish_complete_directories)' -d 'Additional folder with language plugins'
complete -c codecleanup -l languages -d 'List the supported languages'
complete -c codecleanup -l completions -x -a 'bash fish' -d 'Print the completion script for a shell'
complete -c codecleanup -s h -l help -d 'Print help'
complete -c codecleanup -s V -l version -d 'Print version'

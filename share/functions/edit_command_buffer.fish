# localization: tier1
function edit_command_buffer --description 'Edit the command buffer in an external editor'
    if status is-native-windows
        commandline -f edit-command-buffer
        return $status
    end
    __fish_edit_command_buffer fish
end

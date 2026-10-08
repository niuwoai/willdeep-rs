require "fileutils"

# 只做文件操作：原子写入、带时间戳的备份、备份列表与恢复。不打印任何东西。
module ConfigFile
  DEFAULT_MODE = 0600
  DIR_MODE = 0700

  class << self
    def write_atomic(path, text, mode: DEFAULT_MODE)
      dir = File.dirname(File.expand_path(path))
      raise Errno::ENOENT, "目标目录不存在：#{dir}" unless File.directory?(dir)
      target_mode = effective_mode(path, mode)
      tmp = File.join(dir, ".tmp-#{File.basename(path)}-#{Process.pid}-#{rand(1_000_000)}")
      begin
        File.open(tmp, File::WRONLY | File::CREAT | File::EXCL, target_mode) do |f|
          f.binmode
          f.write(text)
          f.flush
          f.fsync
        end
        File.chmod(target_mode, tmp)
        File.rename(tmp, path)
      ensure
        File.unlink(tmp) if File.exist?(tmp)
      end
      File.chmod(target_mode, path)
      path
    end

    def backup(path, backup_dir)
      return nil unless File.file?(path)
      ensure_dir(backup_dir)
      data = File.binread(path)
      dest = next_backup_path(path, backup_dir)
      write_atomic(dest, data, mode: DEFAULT_MODE)
      dest
    end

    def list_backups(backup_dir)
      return [] unless File.directory?(backup_dir)
      out = []
      Dir.glob(File.join(backup_dir, "*.bak")).sort.each do |p|
        next unless File.file?(p)
        st = File.stat(p)
        out << { name: File.basename(p), path: p, size: st.size, mtime: st.mtime }
      end
      out.sort_by { |h| [h[:mtime].to_f, h[:name]] }.reverse
    end

    # 先给现有文件做一次备份，再用备份内容原子覆盖，返回新备份路径
    def restore(backup_path, path)
      raise Errno::ENOENT, "备份文件不存在：#{backup_path}" unless File.file?(backup_path)
      data = File.binread(backup_path)
      new_backup = File.file?(path) ? backup(path, File.dirname(File.expand_path(backup_path))) : nil
      write_atomic(path, data, mode: DEFAULT_MODE)
      new_backup
    end

    def effective_mode(path, mode)
      m = mode
      if File.file?(path)
        cur = File.stat(path).mode & 0777
        m = cur if cur < m
      end
      m
    end

    def ensure_dir(dir)
      return if File.directory?(dir)
      FileUtils.mkdir_p(dir)
      File.chmod(DIR_MODE, dir)
    end

    def next_backup_path(path, backup_dir)
      stamp = Time.now.strftime("%Y%m%d-%H%M%S")
      base = "#{File.basename(path)}-#{stamp}"
      candidate = File.join(backup_dir, "#{base}.bak")
      return candidate unless File.exist?(candidate)
      n = 2
      loop do
        candidate = File.join(backup_dir, "#{base}-#{n}.bak")
        return candidate unless File.exist?(candidate)
        n += 1
      end
    end
  end
end

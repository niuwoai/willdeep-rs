# frozen_string_literal: true
require_relative 'studio_production'
require 'securerandom'

# An evidence journal and gate around the existing production installer.
# It does not change the installer, its frozen tests, or the host registry.
module StudioImprovement
  class Ledger
    def initialize(home)
      @home = File.realpath(home)
      @root = File.join(@home, 'production-loop', StudioProduction::ID)
      @policy = JSON.parse(File.read(File.join(@root, 'policy.json')))
      @source = @policy.fetch('source')
      @journal = File.join(@root, 'improvement.json')
    end

    def locked
      File.open(File.join(@root, 'improvement.lock'), File::RDWR | File::CREAT, 0o600) do |lock|
        raise StudioProduction::Refused, 'another improvement run is active' unless lock.flock(File::LOCK_EX | File::LOCK_NB)
        @data = File.exist?(@journal) ? JSON.parse(File.read(@journal)) : {'schema'=>'willdeep.improvement.v1','events'=>[], 'incidents'=>{}}
        previous = nil
        @data.fetch('events').each do |event|
          copy = event.reject{|key,_|key=='hash'}
          raise StudioProduction::Refused, 'journal integrity failed' unless event['previous']==previous && event['hash']==Digest::SHA256.hexdigest(JSON.generate(copy))
          previous = event['hash']
        end
        head = @data['events'].last
        raise StudioProduction::Refused, 'incident state integrity failed' if head && head['stateHash']!=Digest::SHA256.hexdigest(JSON.generate(@data['incidents']))
        yield self
      end
    end

    def event(action, id, detail = {})
      item = {'sequence'=>@data['events'].length+1,'at'=>Time.now.utc.iso8601,'action'=>action,'incident'=>id,'detail'=>detail,'stateHash'=>Digest::SHA256.hexdigest(JSON.generate(@data['incidents'])),'previous'=>@data['events'].last&.fetch('hash')}
      item['hash'] = Digest::SHA256.hexdigest(JSON.generate(item))
      @data['events'] << item
      StudioProduction.private_json(@journal,@data)
    end

    def bootstrap
      path = File.join(@root,'improvement-policy.json')
      raise StudioProduction::Refused,'improvement policy already exists' if File.exist?(path)
      files = StudioProduction.protected_files(@source)
      files['skills/production-repair/scripts/improvement_ledger.rb'] = Digest::SHA256.file(File.join(@source,'skills/production-repair/scripts/improvement_ledger.rb')).hexdigest
      StudioProduction.private_json(path, {'source'=>@source,'protected'=>files,'originalPolicyHash'=>Digest::SHA256.file(File.join(@root,'policy.json')).hexdigest,'maxCandidates'=>2})
      event('bootstrap',nil)
      {'ok'=>true,'policy'=>path}
    end

    def guard!
      value = JSON.parse(File.read(File.join(@root,'improvement-policy.json')))
      raise StudioProduction::Refused,'original policy changed' unless value['originalPolicyHash']==Digest::SHA256.file(File.join(@root,'policy.json')).hexdigest
      StudioProduction.check_candidate!(@source,@policy)
      value.fetch('protected').each do |relative, hash|
        file = File.join(@source,relative)
        raise StudioProduction::Refused,"improvement verifier changed: #{relative}" unless File.file?(file) && !File.symlink?(file) && Digest::SHA256.file(file).hexdigest==hash
      end
      value
    end

    def self.kind(job)
      return 'creative' if job.dig('result','state')=='needs_human'
      code = job.dig('error','code').to_s
      message = job.dig('error','message').to_s
      return 'dependency' if [code,message].join(' ').match?(/unknownModel|unknownProvider|unauthorized|rate.limit|quota|balance|host_review_unsupported/i)
      # Classification remains unknown until a fixed replay demonstrates a code defect.
      'unknown'
    end

    def scan
      guard!
      client = StudioProduction::Client.new(@home)
      status = client.call('system.status')
      jobs = client.call('jobs.status',{'dramaID'=>@policy.fetch('dramaID'),'limit'=>100,'includeItems'=>false})
      added = []
      jobs.fetch('jobs').each do |job|
        next unless job['state']=='failed' || job.dig('result','state')=='needs_human'
        id = Digest::SHA256.hexdigest(JSON.generate([job['id'],job['updatedAt']]))[0,24]
        next if @data['incidents'].key?(id)
        evidence = File.join(@root,'incidents',id,'observation.json')
        # Private evidence never enters the package, source tree or public report.
        StudioProduction.private_json(evidence,{'job'=>job,'runtime'=>status,'observedAt'=>Time.now.utc.iso8601})
        @data['incidents'][id] = {'id'=>id,'kind'=>self.class.kind(job),'state'=>'observed','jobID'=>job['id'],'dramaID'=>job['dramaID'],'episodeID'=>job['episodeID'],'runtime'=>status,'observationHash'=>Digest::SHA256.file(evidence).hexdigest,'candidates'=>0}
        event('observed',id,{'kind'=>@data['incidents'][id]['kind'],'jobID'=>job['id']})
        added << id
      end
      {'ok'=>true,'added'=>added,'scanned'=>jobs['jobs'].length,'total'=>jobs['total'],'completeScan'=>jobs['jobs'].length==jobs['total']}
    end

    def incident(id, states)
      guard!
      item = @data['incidents'].fetch(id)
      raise StudioProduction::Refused,"invalid transition from #{item['state']}" unless states.include?(item['state'])
      evidence = File.join(@root,'incidents',id,'observation.json')
      raise StudioProduction::Refused,'observation changed' unless Digest::SHA256.file(evidence).hexdigest==item['observationHash']
      item
    end

    def packet_hash(path)
      entries = Dir.glob(File.join(path,'**','*'),File::FNM_DOTMATCH)
      raise StudioProduction::Refused,'replay packet contains links' if File.symlink?(path) || entries.any?{|p|File.symlink?(p)}
      Digest::SHA256.hexdigest(entries.select{|p|File.file?(p)}.sort.map{|p|"#{p.delete_prefix(path+'/')}\0#{Digest::SHA256.file(p).hexdigest}"}.join("\n"))
    end

    def replay(item, mode)
      directory = item.fetch('packet')
      raise StudioProduction::Refused,'fixed replay changed' unless packet_hash(directory)==item.fetch('packetHash')
      args = ['ruby',File.join(directory,'replay.rb'),mode]
      env = {'WILLDEEP_PLUGIN_SOURCE'=>@source,'WILLDEEP_HOME'=>@home,'WILLDEEP_DRAMA_ID'=>@policy['dramaID'],'WILLDEEP_INCIDENT_ID'=>item['id']}
      stdout,stderr,status = Open3.capture3(env,*args,chdir:directory)
      log = File.join(@root,'incidents',item['id'],"#{mode}-#{item['candidates']}.log")
      File.open(log,'w',0o600){|f|f.write(stdout+stderr)}
      value = JSON.parse(stdout)
      expected = mode=='baseline' ? ['failed',1] : ['passed',0]
      raise StudioProduction::Refused,'replay did not produce the required assertion result' unless [value['status'],status.exitstatus]==expected && value['assertionID'].is_a?(String) && !value['assertionID'].empty?
      raise StudioProduction::Refused,'replay assertion changed' if item['assertionID'] && item['assertionID']!=value['assertionID']
      guard!
      raise StudioProduction::Refused,'replay changed its own packet' unless packet_hash(directory)==item['packetHash']
      value.merge('logHash'=>Digest::SHA256.file(log).hexdigest)
    end

    def reproduce(id, packet)
      item = incident(id,['observed'])
      raise StudioProduction::Refused,'dependency or creative feedback is not a code defect' if %w[dependency creative].include?(item['kind'])
      current = StudioProduction::Client.new(@home).call('system.status')
      baseline_hash = StudioProduction.fingerprint(@source)
      raise StudioProduction::Refused,'baseline source is not the running package' unless current==item['runtime'] && baseline_hash==StudioProduction.fingerprint(current['packageRoot'])
      original = File.expand_path(packet)
      raise StudioProduction::Refused,'replay.rb is missing' unless File.file?(File.join(original,'replay.rb'))
      digest = packet_hash(original)
      target = File.join(@root,'incidents',id,'packet')
      raise StudioProduction::Refused,'replay packet already exists' if File.exist?(target)
      FileUtils.cp_r(original,target)
      item['packet'],item['packetHash'] = target,digest
      raise StudioProduction::Refused,'packet copy differs' unless packet_hash(target)==digest
      result = replay(item,'baseline')
      raise StudioProduction::Refused,'baseline source changed during replay' unless baseline_hash==StudioProduction.fingerprint(@source)
      item.merge!('kind'=>'engineering','state'=>'reproduced','assertionID'=>result['assertionID'],'baseline'=>result,'baselineRuntimeHash'=>baseline_hash)
      event('reproduced',id,{'assertionID'=>item['assertionID'],'packetHash'=>digest})
      {'ok'=>true,'incident'=>id,'state'=>item['state']}
    end

    def candidate(id)
      item = incident(id,['reproduced','candidate_failed'])
      raise StudioProduction::Refused,'repair candidate budget exhausted' if item['candidates']>=guard!.fetch('maxCandidates')
      item['candidates'] += 1
      item['state'] = 'candidate_started'
      event('candidate_started',id,{'attempt'=>item['candidates']})
      {'ok'=>true,'attempt'=>item['candidates']}
    end

    def controller(command, *arguments)
      stdout,stderr,status = Open3.capture3('ruby',File.join(@source,'skills/production-repair/scripts/studio_production.rb'),command,'--home',@home,*arguments)
      raise StudioProduction::Refused,"production #{command} failed; #{stderr[-500,500] || stderr}" unless status.success?
      JSON.parse(stdout)
    end

    def verify(id, report)
      item = incident(id,['candidate_started'])
      report = File.expand_path(report)
      begin
        FileUtils.mkdir_p(report)
        additional = guard!.fetch('protected').keys - @policy.fetch('protected').keys
        additional.grep(%r{\Ascripts/[^/]+_test\.rb\z}).each do |path|
          stdout,stderr,status = Open3.capture3('ruby',path,chdir:@source)
          File.open(File.join(report,File.basename(path)+'.log'),'w',0o600){|f|f.write(stdout+stderr)}
          raise StudioProduction::Refused,'new frozen regression failed or skipped' unless status.success? && !stdout.match?(/"status"\s*:\s*"skipped"/)
        end
        controller('verify','--source',@source,'--report',report)
        receipt = JSON.parse(File.read(File.join(report,'receipt.json')))
        raise StudioProduction::Refused,'candidate did not change version and runtime content' if receipt['version']==item['runtime']['version'] || receipt['runtimeHash']==item['baselineRuntimeHash']
        result = replay(item,'candidate')
        raise StudioProduction::Refused,'source changed during replay' unless receipt['runtimeHash']==StudioProduction.fingerprint(@source)
        item.merge!('state'=>'verified','report'=>report,'receiptHash'=>Digest::SHA256.file(File.join(report,'receipt.json')).hexdigest,'candidateResult'=>result,'runtimeHash'=>receipt['runtimeHash'],'version'=>receipt['version'])
        event('verified',id,{'version'=>item['version'],'runtimeHash'=>item['runtimeHash']})
      rescue StandardError
        item['state']='candidate_failed'
        event('candidate_failed',id,{'attempt'=>item['candidates']})
        raise
      end
      {'ok'=>true,'state'=>'verified'}
    end

    def install(id, binary)
      item = incident(id,['verified'])
      raise StudioProduction::Refused,'verification receipt changed' unless Digest::SHA256.file(File.join(item['report'],'receipt.json')).hexdigest==item['receiptHash']
      installer = File.realpath(binary)
      item['state']='install_started'
      event('install_started',id,{'version'=>item['version']})
      begin
        result = controller('install','--source',@source,'--report',item['report'],'--binary',installer)
      rescue StandardError
        item['state']='install_unknown'
        event('install_unknown',id)
        raise
      end
      item['state']='installed_pending_runtime_readback'
      event('installed',id,{'version'=>item['version']})
      result
    end

    def readback(id)
      item = incident(id,['installed_pending_runtime_readback'])
      controller('readback')
      current = StudioProduction::Client.new(@home).call('system.status')
      raise StudioProduction::Refused,'runtime does not match candidate' unless current['version']==item['version'] && StudioProduction.fingerprint(current['packageRoot'])==item['runtimeHash']
      item.merge!('state'=>'runtime_verified','runtime'=>current)
      event('runtime_verified',id,{'version'=>current['version']})
      {'ok'=>true,'state'=>item['state']}
    end

    def resolve(id)
      item = incident(id,['runtime_verified'])
      current = StudioProduction::Client.new(@home).call('system.status')
      raise StudioProduction::Refused,'runtime changed before recovery validation' unless current==item['runtime'] && StudioProduction.fingerprint(current['packageRoot'])==item['runtimeHash']
      result = replay(item,'live')
      artifact = result.fetch('artifact')
      path = artifact.fetch('path')
      raise StudioProduction::Refused,'live artifact missing or mismatched' unless result['incidentID']==id && result['dramaID']==@policy['dramaID'] && File.absolute_path(path)==path && File.file?(path) && Digest::SHA256.file(path).hexdigest==artifact['sha256']
      after = StudioProduction::Client.new(@home).call('system.status')
      raise StudioProduction::Refused,'runtime changed during recovery validation' unless after==current && StudioProduction.fingerprint(current['packageRoot'])==item['runtimeHash']
      item.merge!('state'=>'resolved','liveResult'=>result)
      event('resolved',id,{'artifactHash'=>artifact['sha256']})
      {'ok'=>true,'state'=>'resolved','artifact'=>artifact}
    end

    def status
      guard!
      {'ok'=>true,'incidents'=>@data['incidents'].values.map{|i|i.reject{|k,_|%w[baseline candidateResult liveResult runtime].include?(k)}},'events'=>@data['events'].length,'head'=>@data['events'].last&.fetch('hash')}
    end
  end

  def self.main(argv)
    command = argv.shift
    options = {'home'=>File.expand_path('~/.willdeep'),'binary'=>'/opt/homebrew/bin/willdeep'}
    OptionParser.new do |opt|
      %w[home incident packet report binary].each{|key|opt.on("--#{key} VALUE"){|v|options[key]=v}}
    end.parse!(argv)
    raise StudioProduction::Refused,'unexpected arguments' unless argv.empty?
    result = Ledger.new(options['home']).locked do |ledger|
      case command
      when 'bootstrap','scan','status' then ledger.public_send(command)
      when 'reproduce' then ledger.reproduce(options.fetch('incident'),options.fetch('packet'))
      when 'candidate' then ledger.candidate(options.fetch('incident'))
      when 'verify' then ledger.verify(options.fetch('incident'),options.fetch('report'))
      when 'install' then ledger.install(options.fetch('incident'),options.fetch('binary'))
      when 'readback','resolve' then ledger.public_send(command,options.fetch('incident'))
      else raise StudioProduction::Refused,'use bootstrap, scan, status, reproduce, candidate, verify, install, readback, resolve'
      end
    end
    puts JSON.generate(result)
  end
end

if $PROGRAM_NAME==__FILE__
  begin
    StudioImprovement.main(ARGV)
  rescue StandardError => error
    warn JSON.generate('ok'=>false,'error'=>error.class.name,'message'=>error.message)
    exit 1
  end
end
